//! The fingerprint report (`LightBrowser.fingerprintReport`,
//! `lightbrowser fingerprint-report`): the live surface of a page compared
//! with its persona.
//!
//! - **Collect** ([`collect`]): `fingerprint.js` runs in a throwaway
//!   isolated world of the page's current document, created for the report
//!   and dropped afterwards (never reported to CDP clients, nothing runs in
//!   the main world). It probes navigator, UA client hints, screen, window
//!   sizes, time zone and locale, WebGL, canvas, audio, fonts (width
//!   measurement of the persona's list), permissions, battery, media
//!   devices, voices, media queries and `performance.now` resolution, and
//!   the navigator, UA client hints, WebGPU presence, time zone and locale
//!   as a dedicated, a shared and a service worker see them (ADR 0021,
//!   0022; the service worker runs outside the registry). The
//!   main world's global is inspected natively for automation tells.
//! - **Compare** ([`diff`]): one check per value, with status `ok`,
//!   `mismatch`, `unimplemented` (the surface is missing or threw) or
//!   `info` (hashes). The report is `ok` when nothing mismatches.

use std::rc::Rc;
use std::time::Duration;

use lb_js::{PromiseOutcome, ScriptOrigin};
use lb_persona::{BrowserFamily, PersonaProfile};
use serde_json::{Value, json};

use crate::browser::WorkerState;
use crate::page::PageState;

const COLLECTOR: &str = include_str!("fingerprint.js");

/// Name of the throwaway world the collector runs in.
const WORLD: &str = "__lightbrowser_fingerprint__";

/// How long the collector may take (promises: permissions, audio, UA-CH).
const TIMEOUT: Duration = Duration::from_secs(20);

/// Main-world global names that give automation away.
const TELLS: &[&str] = &[
    "webdriver",
    "cdc_",
    "$cdc",
    "__playwright",
    "__pw",
    "playwright",
    "puppeteer",
    "__puppeteer",
    "domAutomation",
    "callPhantom",
    "_phantom",
    "__nightmare",
    "_selenium",
    "__selenium",
    "__webdriver",
    "__lightbrowser",
    "__driver",
    "_Selenium_IDE",
];

/// Fonts no persona lists; detecting one means the font check is broken.
const ABSENT_FONTS: &[&str] = &["LightBrowser No Such Font", "Zzyzx Sans Fake"];

/// The collector's configuration for `profile`.
fn config(profile: &PersonaProfile) -> Value {
    json!({
        "fonts": profile.fonts.available,
        "absentFonts": ABSENT_FONTS,
        "permissions": profile.permissions.keys().collect::<Vec<_>>(),
        "webglParams": profile.webgl.parameters.keys().collect::<Vec<_>>(),
        "shaderPrecision": profile.webgl.shader_precision.keys().collect::<Vec<_>>(),
        "genericFonts": generic_fonts(profile),
    })
}

/// Generic keyword -> the persona's default family, for the collector.
/// macOS `system-ui` (SF) has no family name a page can write, so it is
/// left out.
fn generic_fonts(profile: &PersonaProfile) -> Value {
    let d = profile.font_defaults();
    let mut m = serde_json::Map::new();
    for (k, v) in [
        ("serif", &d.serif),
        ("sans-serif", &d.sans_serif),
        ("monospace", &d.monospace),
        ("system-ui", &d.system_ui),
    ] {
        if v != "system-ui" {
            m.insert(k.to_owned(), json!(v));
        }
    }
    Value::Object(m)
}

/// Collect the live surface of the page's current document (see the
/// module docs). Runs on the page's worker.
pub(crate) async fn collect(
    ws: &Rc<WorkerState>,
    page: &Rc<PageState>,
    profile: &PersonaProfile,
) -> Result<Value, String> {
    let w = page.window().ok_or("page is closed")?;
    let (rt, bindings) = ws.runtime().ok_or("page is closed")?;
    let src = format!("const CFG = {};\n{COLLECTOR}", config(profile));
    let realm = ws
        .with_engine(|e| w.create_isolated_world(e, &bindings, WORLD))
        .ok_or("page is closed")?
        .map_err(|e| e.message)?;
    let result = run_collector(ws, page, realm, &src).await;
    // The main world's global, read natively.
    let globals = ws
        .with_engine(|e| -> Result<Vec<String>, String> {
            let main = w.realm();
            let g = e.global(main).map_err(|e| e.message)?;
            let props = e
                .own_properties(main, g, true, false)
                .map_err(|e| e.message)?;
            let mut names = Vec::with_capacity(props.len());
            for p in props {
                for h in [p.value, p.get, p.set, p.symbol].into_iter().flatten() {
                    e.release(h);
                }
                names.push(p.name);
            }
            e.release(g);
            Ok(names)
        })
        .ok_or("page is closed")??;
    rt.detach(realm);
    ws.with_engine(|e| e.drop_realm(realm));
    let mut surface: Value =
        serde_json::from_str(&result?).map_err(|e| format!("collector output: {e}"))?;
    let tells: Vec<&String> = globals
        .iter()
        .filter(|n| {
            let l = n.to_ascii_lowercase();
            TELLS.iter().any(|t| l.contains(&t.to_ascii_lowercase()))
        })
        .collect();
    surface["mainWorld"] = json!({
        "globals": globals.len(),
        "tells": tells,
        "chrome": globals.iter().any(|n| n == "chrome"),
    });
    surface["url"] = json!(w.url().to_string());
    // A service worker sees the same persona (ADR 0022): probed in a
    // version started outside the registry (nothing the page can see).
    if surface["workers"].is_object() {
        surface["workers"]["service"] = probe_service_worker(page, &w.url()).await;
    }
    Ok(surface)
}

/// The worker probe's values in a `ServiceWorkerGlobalScope`.
const SW_PROBE: &str = "(async function v() { const n = navigator, d = n.userAgentData, o = Intl.DateTimeFormat().resolvedOptions();\
const r = { userAgent: n.userAgent, appVersion: n.appVersion, platform: n.platform, language: n.language,\
languages: Array.from(n.languages || []), hardwareConcurrency: n.hardwareConcurrency,\
deviceMemory: n.deviceMemory === undefined ? null : n.deviceMemory, gpu: 'gpu' in n,\
timeZone: o.timeZone, locale: o.locale };\
if (d) { r.uaData = { brands: d.brands.map((b) => ({ brand: b.brand, version: b.version })), mobile: d.mobile, platform: d.platform };\
const h = await d.getHighEntropyValues(['architecture', 'bitness', 'model', 'platformVersion', 'uaFullVersion', 'fullVersionList', 'wow64', 'formFactors']);\
r.uaHigh = { architecture: h.architecture, bitness: h.bitness, model: h.model, platformVersion: h.platformVersion,\
uaFullVersion: h.uaFullVersion, wow64: h.wow64, formFactors: h.formFactors,\
fullVersionList: (h.fullVersionList || []).map((b) => ({ brand: b.brand, version: b.version })) }; }\
return JSON.stringify(r); })()";

/// Run [`SW_PROBE`] in a service worker scope of the page's context
/// (its own thread and isolate, never registered, ended afterwards).
async fn probe_service_worker(page: &Rc<PageState>, url: &url::Url) -> Value {
    use lb_webapi::rt::{WorkerControl, WorkerKind, WorkerStart, sw, worker};
    let ctx = page.ctx.clone();
    let (inbox, inbox_rx) = worker::inbox();
    let control = WorkerControl::new(inbox.clone());
    let (etx, erx) = tokio::sync::oneshot::channel();
    let start = WorkerStart {
        kind: WorkerKind::Service,
        url: url
            .join("/__lb_probe_sw.js")
            .unwrap_or_else(|_| url.clone()),
        source: Some(String::new()),
        module: false,
        name: String::new(),
        origin: url.origin().ascii_serialization(),
        creator: url.clone(),
        persona: ctx.persona().clone(),
        profile: ctx.profile().cloned(),
        virtual_time: false,
        inbox: inbox.clone(),
        inbox_rx,
        control: control.clone(),
        parent: None,
        service: Some(sw::ServiceStart {
            version: 0,
            registration: 0,
            evaluated: Box::new(move |r| {
                let _ = etx.send(r);
            }),
        }),
    };
    crate::worker::spawn(ctx, start);
    let run = async {
        erx.await
            .map_err(|_| "service worker did not start".to_owned())?
            .map_err(|()| "service worker script failed".to_owned())?;
        let (tx, rx) = tokio::sync::oneshot::channel();
        inbox.send(sw::eval_task(
            SW_PROBE.to_owned(),
            Box::new(move |r| {
                let _ = tx.send(r);
            }),
        ));
        rx.await.map_err(|_| "service worker ended".to_owned())?
    };
    let r = tokio::time::timeout(Duration::from_secs(5), run).await;
    control.terminate();
    match r {
        Ok(Ok(s)) => {
            serde_json::from_str(&s).unwrap_or_else(|e| json!({ "__error": e.to_string() }))
        }
        Ok(Err(e)) => json!({ "__error": e }),
        Err(_) => json!({ "__error": "service worker timeout" }),
    }
}

/// Evaluate the collector and wait for its promise.
async fn run_collector(
    ws: &Rc<WorkerState>,
    page: &Rc<PageState>,
    realm: lb_js::RealmId,
    src: &str,
) -> Result<String, String> {
    let h = ws
        .with_engine(|e| {
            let r = e.evaluate_handle(realm, src, ScriptOrigin::new(""));
            e.perform_microtask_checkpoint();
            r
        })
        .ok_or("page is closed")?
        .map_err(|e| format!("collector threw: {}", e.message))?;
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let out = ws
            .with_engine(|e| {
                e.perform_microtask_checkpoint();
                match e.promise_outcome(realm, h) {
                    Ok(PromiseOutcome::Pending) => Ok(None),
                    Ok(PromiseOutcome::Fulfilled(v)) => {
                        let r = e.get_value(realm, v);
                        e.release(v);
                        r.map(Some).map_err(|e| e.message)
                    }
                    Ok(PromiseOutcome::Rejected(v)) => {
                        let r = e.get_value(realm, v);
                        e.release(v);
                        Err(format!("collector rejected: {r:?}"))
                    }
                    Ok(PromiseOutcome::NotAPromise) => {
                        e.get_value(realm, h).map(Some).map_err(|e| e.message)
                    }
                    Err(err) => Err(err.message),
                }
            })
            .ok_or("page is closed")?;
        match out? {
            Some(lb_js::JsValue::String(s)) => {
                ws.with_engine(|e| e.release(h));
                return Ok(s);
            }
            Some(v) => return Err(format!("collector returned {v:?}")),
            None => {}
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("collector timed out".into());
        }
        tokio::select! {
            _ = page.turn.notified() => {}
            _ = tokio::time::sleep(Duration::from_millis(5)) => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Diff
// ---------------------------------------------------------------------------

/// Builds the check list.
struct Checks {
    list: Vec<Value>,
    counts: [u32; 4],
}

const OK: usize = 0;
const MISMATCH: usize = 1;
const UNIMPLEMENTED: usize = 2;
const INFO: usize = 3;
const STATUS: [&str; 4] = ["ok", "mismatch", "unimplemented", "info"];

/// A value of the surface, or why it is missing.
fn lookup<'a>(surface: &'a Value, path: &str) -> Result<&'a Value, String> {
    let mut cur = surface;
    let mut at = String::new();
    for part in path.split('.') {
        if let Some(e) = cur.get("__error") {
            return Err(format!("{at}: {}", e.as_str().unwrap_or("error")));
        }
        at = if at.is_empty() {
            part.to_string()
        } else {
            format!("{at}.{part}")
        };
        cur = match cur.get(part) {
            Some(v) => v,
            None if cur.is_null() => return Err(format!("{at}: not available")),
            None => return Err(format!("{at}: missing")),
        };
    }
    if let Some(e) = cur.get("__error") {
        return Err(format!("{path}: {}", e.as_str().unwrap_or("error")));
    }
    Ok(cur)
}

/// Numbers compare by value (`8` equals `8.0`).
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| same(a, b))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w)))
        }
        _ => a == b,
    }
}

impl Checks {
    fn push(
        &mut self,
        name: &str,
        status: usize,
        expected: Value,
        actual: Value,
        note: Option<String>,
    ) {
        self.counts[status] += 1;
        let mut c = json!({
            "name": name,
            "status": STATUS[status],
            "expected": expected,
            "actual": actual,
        });
        if let Some(n) = note {
            c["note"] = json!(n);
        }
        self.list.push(c);
    }

    /// `path` of the surface must equal `expected`.
    fn eq(&mut self, s: &Value, name: &str, path: &str, expected: Value) {
        self.with(s, name, path, expected, same);
    }

    /// `path` must satisfy `ok(actual, expected)`.
    fn with(
        &mut self,
        s: &Value,
        name: &str,
        path: &str,
        expected: Value,
        ok: impl Fn(&Value, &Value) -> bool,
    ) {
        match lookup(s, path) {
            Ok(a) => {
                let st = if ok(a, &expected) { OK } else { MISMATCH };
                self.push(name, st, expected, a.clone(), None);
            }
            Err(why) => self.push(name, UNIMPLEMENTED, expected, Value::Null, Some(why)),
        }
    }

    fn info(&mut self, s: &Value, name: &str, path: &str) {
        match lookup(s, path) {
            Ok(a) => self.push(name, INFO, Value::Null, a.clone(), None),
            Err(why) => self.push(name, UNIMPLEMENTED, Value::Null, Value::Null, Some(why)),
        }
    }
}

/// Chrome's fixed PDF plugin list.
const CHROME_PLUGINS: &[&str] = &[
    "PDF Viewer",
    "Chrome PDF Viewer",
    "Chromium PDF Viewer",
    "Microsoft Edge PDF Viewer",
    "WebKit built-in PDF",
];

/// Compare a collected surface with the persona. Returns the report.
pub fn diff(p: &PersonaProfile, s: &Value) -> Value {
    let mut c = Checks {
        list: Vec::new(),
        counts: [0; 4],
    };
    let chrome = p.browser.family == BrowserFamily::Chrome;
    let b = &p.browser;

    // Navigator.
    c.eq(
        s,
        "navigator.userAgent",
        "navigator.userAgent",
        json!(b.user_agent),
    );
    c.eq(
        s,
        "navigator.appVersion",
        "navigator.appVersion",
        json!(b.app_version),
    );
    c.eq(
        s,
        "navigator.appName",
        "navigator.appName",
        json!(b.app_name),
    );
    c.eq(
        s,
        "navigator.appCodeName",
        "navigator.appCodeName",
        json!(b.app_code_name),
    );
    c.eq(
        s,
        "navigator.platform",
        "navigator.platform",
        json!(p.os.platform),
    );
    c.eq(
        s,
        "navigator.product",
        "navigator.product",
        json!(b.product),
    );
    c.eq(
        s,
        "navigator.productSub",
        "navigator.productSub",
        json!(b.product_sub),
    );
    c.eq(s, "navigator.vendor", "navigator.vendor", json!(b.vendor));
    c.eq(
        s,
        "navigator.vendorSub",
        "navigator.vendorSub",
        json!(b.vendor_sub),
    );
    c.eq(
        s,
        "navigator.language",
        "navigator.language",
        json!(p.locale.language),
    );
    c.eq(
        s,
        "navigator.languages",
        "navigator.languages",
        json!(p.locale.languages),
    );
    c.eq(
        s,
        "navigator.hardwareConcurrency",
        "navigator.hardwareConcurrency",
        json!(p.hardware.hardware_concurrency),
    );
    c.eq(
        s,
        "navigator.deviceMemory",
        "navigator.deviceMemory",
        json!(p.hardware.device_memory),
    );
    c.eq(
        s,
        "navigator.maxTouchPoints",
        "navigator.maxTouchPoints",
        json!(p.hardware.max_touch_points),
    );
    c.eq(
        s,
        "navigator.cookieEnabled",
        "navigator.cookieEnabled",
        json!(b.cookie_enabled),
    );
    c.eq(
        s,
        "navigator.pdfViewerEnabled",
        "navigator.pdfViewerEnabled",
        json!(b.pdf_viewer_enabled),
    );
    c.eq(
        s,
        "navigator.doNotTrack",
        "navigator.doNotTrack",
        json!(b.do_not_track),
    );
    if chrome {
        let plugins: &[&str] = if b.pdf_viewer_enabled {
            CHROME_PLUGINS
        } else {
            &[]
        };
        c.eq(s, "navigator.plugins", "navigator.plugins", json!(plugins));
    }

    // UA client hints.
    if let Some(ch) = &p.client_hints {
        let brands = |l: &[lb_persona::BrandVersion]| -> Value {
            l.iter()
                .map(|x| json!({"brand": x.brand, "version": x.version}))
                .collect()
        };
        c.eq(
            s,
            "userAgentData.brands",
            "uaData.brands",
            brands(&ch.brands),
        );
        c.eq(s, "userAgentData.mobile", "uaData.mobile", json!(ch.mobile));
        c.eq(
            s,
            "userAgentData.platform",
            "uaData.platform",
            json!(ch.platform),
        );
        c.eq(
            s,
            "highEntropy.architecture",
            "uaHigh.architecture",
            json!(ch.architecture),
        );
        c.eq(
            s,
            "highEntropy.bitness",
            "uaHigh.bitness",
            json!(ch.bitness),
        );
        c.eq(s, "highEntropy.model", "uaHigh.model", json!(ch.model));
        c.eq(
            s,
            "highEntropy.platformVersion",
            "uaHigh.platformVersion",
            json!(ch.platform_version),
        );
        c.eq(s, "highEntropy.wow64", "uaHigh.wow64", json!(ch.wow64));
        c.eq(
            s,
            "highEntropy.fullVersionList",
            "uaHigh.fullVersionList",
            brands(&ch.full_version_list),
        );
        if let Some(he) = p.high_entropy_hints() {
            c.eq(
                s,
                "highEntropy.formFactors",
                "uaHigh.formFactors",
                json!(he.form_factors),
            );
        }
        if let Some(v) = &ch.full_version {
            c.eq(
                s,
                "highEntropy.uaFullVersion",
                "uaHigh.uaFullVersion",
                json!(v),
            );
        }
    }

    // Network information (Chrome rounds and adds per-host noise).
    if chrome {
        let near = |tol: f64| {
            move |a: &Value, e: &Value| match (a.as_f64(), e.as_f64()) {
                (Some(a), Some(e)) => (a - e).abs() <= (e * tol).max(25.0),
                _ => false,
            }
        };
        c.eq(
            s,
            "connection.effectiveType",
            "connection.effectiveType",
            json!(p.network.connection_type),
        );
        c.eq(
            s,
            "connection.saveData",
            "connection.saveData",
            json!(p.network.save_data),
        );
        c.with(
            s,
            "connection.rtt",
            "connection.rtt",
            json!(p.network.rtt),
            near(0.2),
        );
        c.with(
            s,
            "connection.downlink",
            "connection.downlink",
            json!(p.network.downlink),
            |a, e| match (a.as_f64(), e.as_f64()) {
                (Some(a), Some(e)) => (a - e).abs() <= e * 0.2 + 0.05,
                _ => false,
            },
        );
    }

    // Screen and window.
    let sc = &p.screen;
    c.eq(s, "screen.width", "screen.width", json!(sc.width));
    c.eq(s, "screen.height", "screen.height", json!(sc.height));
    c.eq(
        s,
        "screen.availWidth",
        "screen.availWidth",
        json!(sc.avail_width),
    );
    c.eq(
        s,
        "screen.availHeight",
        "screen.availHeight",
        json!(sc.avail_height),
    );
    c.eq(s, "screen.availTop", "screen.availTop", json!(sc.avail_top));
    c.eq(
        s,
        "screen.availLeft",
        "screen.availLeft",
        json!(sc.avail_left),
    );
    c.eq(
        s,
        "screen.colorDepth",
        "screen.colorDepth",
        json!(sc.color_depth),
    );
    c.eq(
        s,
        "screen.pixelDepth",
        "screen.pixelDepth",
        json!(sc.pixel_depth),
    );
    c.eq(
        s,
        "window.devicePixelRatio",
        "screen.devicePixelRatio",
        json!(sc.device_pixel_ratio),
    );
    c.eq(
        s,
        "window.innerWidth",
        "window.innerWidth",
        json!(sc.inner_width),
    );
    c.eq(
        s,
        "window.innerHeight",
        "window.innerHeight",
        json!(sc.inner_height),
    );
    c.eq(
        s,
        "window.outerWidth",
        "window.outerWidth",
        json!(sc.outer_width),
    );
    c.eq(
        s,
        "window.outerHeight",
        "window.outerHeight",
        json!(sc.outer_height),
    );

    // OS axis (M7): window position, scrollbars, generic fonts, OS-gated
    // APIs, codecs, WebGPU.
    let (wx, wy) = p.window_position();
    c.eq(s, "window.screenX", "window.screenX", json!(wx));
    c.eq(s, "window.screenY", "window.screenY", json!(wy));
    if chrome {
        c.eq(
            s,
            "layout.scrollbarWidth",
            "layout.scrollbarWidth",
            json!(p.scrollbar_width()),
        );
        if let Some(g) = generic_fonts(p).as_object() {
            for k in g.keys() {
                c.eq(
                    s,
                    &format!("fonts.generic.{k}"),
                    &format!("genericFonts.{k}"),
                    json!(true),
                );
            }
        }
        let apis = p.os_apis();
        c.eq(s, "navigator.share", "osApis.share", json!(apis.share));
        c.eq(
            s,
            "navigator.canShare",
            "osApis.canShare",
            json!(apis.share),
        );
        c.eq(
            s,
            "navigator.bluetooth",
            "osApis.bluetooth",
            json!(apis.bluetooth),
        );
        c.eq(
            s,
            "window.Bluetooth",
            "osApis.Bluetooth",
            json!(apis.bluetooth),
        );
        let codecs = p.codecs();
        c.eq(s, "media.hevc", "codecs.hevc", json!(codecs.hevc));
        c.eq(
            s,
            "media.h264PowerEfficient",
            "codecs.h264PowerEfficient",
            json!(codecs.h264_power_efficient),
        );
        c.eq(
            s,
            "media.av1PowerEfficient",
            "codecs.av1PowerEfficient",
            json!(codecs.av1_hw),
        );
        let gpu = p
            .webgpu()
            .map(|a| json!({"vendor": a.vendor, "architecture": a.architecture}));
        c.eq(s, "webgpu.adapter", "webgpu", gpu.unwrap_or(Value::Null));
    }

    // Time zone and locale.
    c.eq(
        s,
        "intl.timeZone",
        "locale.timeZone",
        json!(p.locale.timezone),
    );
    c.eq(s, "intl.locale", "locale.locale", json!(p.locale.language));
    c.info(s, "date.timezoneOffset", "locale.offset");

    // Workers (ADR 0021, 0022): a dedicated, a shared and a service
    // worker see the same persona as the page (navigator, UA client hints,
    // WebGPU presence, time zone and locale).
    if chrome {
        for scope in ["dedicated", "shared", "service"] {
            let w = |k: &str| format!("workers.{scope}.{k}");
            let n = |k: &str| format!("worker.{scope}.{k}");
            for (k, v) in [
                ("userAgent", json!(b.user_agent)),
                ("appVersion", json!(b.app_version)),
                ("platform", json!(p.os.platform)),
                ("language", json!(p.locale.language)),
                ("languages", json!(p.locale.languages)),
                (
                    "hardwareConcurrency",
                    json!(p.hardware.hardware_concurrency),
                ),
                ("deviceMemory", json!(p.hardware.device_memory)),
                ("timeZone", json!(p.locale.timezone)),
                ("locale", json!(p.locale.language)),
            ] {
                c.eq(s, &n(k), &w(k), v);
            }
            // `navigator.gpu` exists in workers wherever the page has it.
            let page_gpu = lookup(s, "webgpu").is_ok_and(|v| !v.is_null()) || p.webgpu().is_some();
            c.eq(s, &n("gpu"), &w("gpu"), json!(page_gpu || chrome));
            if let Some(ch) = &p.client_hints {
                let brands = |l: &[lb_persona::BrandVersion]| -> Value {
                    l.iter()
                        .map(|x| json!({"brand": x.brand, "version": x.version}))
                        .collect()
                };
                c.eq(
                    s,
                    &n("userAgentData.brands"),
                    &w("uaData.brands"),
                    brands(&ch.brands),
                );
                c.eq(
                    s,
                    &n("userAgentData.mobile"),
                    &w("uaData.mobile"),
                    json!(ch.mobile),
                );
                c.eq(
                    s,
                    &n("userAgentData.platform"),
                    &w("uaData.platform"),
                    json!(ch.platform),
                );
                for (k, v) in [
                    ("architecture", json!(ch.architecture)),
                    ("bitness", json!(ch.bitness)),
                    ("model", json!(ch.model)),
                    ("platformVersion", json!(ch.platform_version)),
                    ("wow64", json!(ch.wow64)),
                    ("fullVersionList", brands(&ch.full_version_list)),
                ] {
                    c.eq(
                        s,
                        &n(&format!("highEntropy.{k}")),
                        &w(&format!("uaHigh.{k}")),
                        v,
                    );
                }
                if let Some(he) = p.high_entropy_hints() {
                    c.eq(
                        s,
                        &n("highEntropy.formFactors"),
                        &w("uaHigh.formFactors"),
                        json!(he.form_factors),
                    );
                }
                if let Some(v) = &ch.full_version {
                    c.eq(
                        s,
                        &n("highEntropy.uaFullVersion"),
                        &w("uaHigh.uaFullVersion"),
                        json!(v),
                    );
                }
            }
        }
    }

    // WebGL (a WebGL 2 context: the persona lists its version strings).
    let g = &p.hardware.gpu;
    c.eq(s, "webgl.vendor", "webgl.vendor", json!(g.vendor));
    c.eq(s, "webgl.renderer", "webgl.renderer", json!(g.renderer));
    c.eq(
        s,
        "webgl.unmaskedVendor",
        "webgl.unmaskedVendor",
        json!(g.vendor_unmasked),
    );
    c.eq(
        s,
        "webgl.unmaskedRenderer",
        "webgl.unmaskedRenderer",
        json!(g.renderer_unmasked),
    );
    c.eq(s, "webgl.version", "webgl.version", json!(p.webgl.version));
    c.eq(
        s,
        "webgl.shadingLanguageVersion",
        "webgl.glsl",
        json!(p.webgl.shading_language_version),
    );
    c.eq(
        s,
        "webgl.extensions",
        "webgl.extensions",
        json!(p.webgl.extensions),
    );
    for (k, v) in &p.webgl.parameters {
        let path = format!("webgl.params.{k}");
        c.eq(s, &format!("webgl.{k}"), &path, v.clone());
    }
    for (k, v) in &p.webgl.shader_precision {
        // `VERTEX_SHADER.LOW_FLOAT` is `VERTEX_SHADER:LOW_FLOAT` in the surface.
        let path = format!("webgl.precision.{}", k.replace('.', ":"));
        c.eq(
            s,
            &format!("webgl.precision.{k}"),
            &path,
            json!({"range_min": v.range_min, "range_max": v.range_max, "precision": v.precision}),
        );
    }

    // Canvas and audio: stable, seeded; hashes are informational.
    c.eq(s, "canvas.stable", "canvas.stable", json!(true));
    c.eq(s, "canvas.solidFillExact", "canvas.solidExact", json!(true));
    c.info(s, "canvas.hash", "canvas.hash");
    c.eq(s, "webgl.pixelStable", "webgl.pixelStable", json!(true));
    c.info(s, "webgl.pixelHash", "webgl.pixelHash");
    c.info(s, "audio.sum", "audio.sum");
    c.info(s, "audio.hash", "audio.hash");
    c.info(s, "fonts.hash", "fonts.hash");
    // Realtime audio defaults (the persona's `audio`, else the OS's).
    let ad = p.audio_defaults();
    c.eq(
        s,
        "audio.sampleRate",
        "audio.live.sampleRate",
        json!(ad.sample_rate),
    );
    c.eq(
        s,
        "audio.baseLatency",
        "audio.live.baseLatency",
        json!(ad.base_latency),
    );
    c.eq(
        s,
        "audio.maxChannelCount",
        "audio.live.maxChannelCount",
        json!(ad.channels),
    );
    c.with(
        s,
        "audio.outputLatency",
        "audio.live.outputLatency",
        json!(ad.output_latency),
        |a, e| a.is_null() || same(a, e),
    );

    // Fonts: every persona font is detected, made-up ones are not.
    c.eq(s, "fonts.missing", "fonts.missing", json!([]));
    c.eq(s, "fonts.unexpected", "fonts.unexpected", json!([]));

    // Permissions and notifications.
    for (name, state) in &p.permissions {
        c.eq(
            s,
            &format!("permissions.{name}"),
            &format!("permissions.{name}"),
            json!(state),
        );
    }
    if let Some(n) = p.permissions.get("notifications") {
        let want = if n == "prompt" { "default" } else { n.as_str() };
        c.eq(s, "Notification.permission", "notification", json!(want));
    }
    if let Some(bat) = &p.battery {
        c.eq(s, "battery.level", "battery.level", json!(bat.level));
        c.eq(
            s,
            "battery.charging",
            "battery.charging",
            json!(bat.charging),
        );
    } else {
        c.info(s, "battery", "battery");
    }
    if let Some(m) = &p.media_devices {
        for (kind, n) in [
            ("audioinput", m.audioinput),
            ("videoinput", m.videoinput),
            ("audiooutput", m.audiooutput),
        ] {
            if let Some(n) = n {
                let path = format!("mediaDevices.{kind}");
                c.with(
                    s,
                    &format!("mediaDevices.{kind}"),
                    &path,
                    json!(n),
                    |a, e| same(a, e) || (a.is_null() && e.as_u64() == Some(0)),
                );
            }
        }
    }
    if !p.speech_voices.is_empty() {
        let names: Vec<&str> = p.speech_voices.iter().map(|v| v.name.as_str()).collect();
        c.eq(s, "speechSynthesis.voices", "voices", json!(names));
    }
    if chrome {
        c.with(
            s,
            "speechSynthesis.voiceCount",
            "voices",
            json!(p.speech_voices.len()),
            |a, e| a.as_array().map(Vec::len) == e.as_u64().map(|n| n as usize),
        );
    }
    if let Some(pr) = &p.preferences {
        c.eq(
            s,
            "media.prefersDark",
            "media.dark",
            json!(pr.color_scheme == "dark"),
        );
        c.eq(
            s,
            "media.reducedMotion",
            "media.reducedMotion",
            json!(pr.reduced_motion),
        );
        c.eq(
            s,
            "media.colorGamutP3",
            "media.p3",
            json!(pr.color_gamut != "srgb"),
        );
    }
    if chrome {
        c.eq(
            s,
            "performance.now100us",
            "performance.tenthMs",
            json!(true),
        );
    }

    // No tells.
    c.eq(s, "noTells.webdriver", "navigator.webdriver", json!(false));
    c.eq(
        s,
        "noTells.webdriverOwnProperty",
        "navigator.webdriverOwn",
        json!(false),
    );
    c.eq(s, "noTells.mainWorldGlobals", "mainWorld.tells", json!([]));
    c.eq(s, "noTells.windowChrome", "mainWorld.chrome", json!(chrome));

    let [ok, mismatch, unimplemented, info] = c.counts;
    json!({
        "persona": p.profile_id,
        "family": p.browser.family.to_string(),
        "url": s.get("url").cloned().unwrap_or(Value::Null),
        "ok": mismatch == 0,
        "summary": {
            "ok": ok,
            "mismatch": mismatch,
            "unimplemented": unimplemented,
            "info": info,
        },
        "checks": c.list,
    })
}

/// The report for the page's current document (see the module docs).
pub(crate) async fn report(ws: &Rc<WorkerState>, page: &Rc<PageState>) -> Result<Value, String> {
    let profile = page
        .ctx
        .profile()
        .cloned()
        .ok_or("the context has no full persona (PersonaProfile)")?;
    let surface = collect(ws, page, &profile).await?;
    let mut r = diff(&profile, &surface);
    r["surface"] = surface;
    Ok(r)
}

impl crate::Page {
    /// The fingerprint report of the current document (see
    /// [`crate::fingerprint`]): `report["ok"]` is false when a surface
    /// contradicts the persona.
    pub async fn fingerprint_report(&self) -> Result<Value, crate::PageError> {
        self.call(
            |ws, page| async move { report(&ws, &page).await.map_err(crate::PageError::Setup) },
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_reports_errors_and_missing() {
        let s = json!({"a": {"b": 1}, "e": {"__error": "boom"}, "n": null});
        assert_eq!(lookup(&s, "a.b"), Ok(&json!(1)));
        assert!(lookup(&s, "e.x").unwrap_err().contains("boom"));
        assert!(lookup(&s, "n.x").unwrap_err().contains("not available"));
        assert!(lookup(&s, "a.c").unwrap_err().contains("missing"));
        assert!(same(&json!(8), &json!(8.0)));
        assert!(same(&json!({"x": [1, 2]}), &json!({"x": [1.0, 2.0]})));
    }

    #[test]
    fn missing_surfaces_are_unimplemented_and_wrong_values_mismatch() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../profiles/reference/chrome/chrome153_win10_rtx3060_1080p.json"
        );
        let p = PersonaProfile::from_path_checked(std::path::Path::new(path)).unwrap();
        let r = diff(&p, &json!({"webgl": {"__error": "no WebGL"}}));
        assert_eq!(r["ok"], true);
        assert_eq!(r["summary"]["mismatch"], 0);
        assert!(r["summary"]["unimplemented"].as_u64().unwrap() > 100);
        let s = json!({"navigator": {"userAgent": "HeadlessChrome/153", "webdriver": true}});
        let r = diff(&p, &s);
        assert_eq!(r["ok"], false);
        let bad: Vec<&str> = r["checks"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["status"] == "mismatch")
            .map(|c| c["name"].as_str().unwrap())
            .collect();
        assert_eq!(bad, ["navigator.userAgent", "noTells.webdriver"]);
    }
}

//! The navigator fingerprint surface (chrome build), all from the persona:
//! `plugins` / `mimeTypes` (Chrome's fixed PDF set), `userAgentData`,
//! `connection`, `permissions`, `Notification`, `getBattery`,
//! `mediaDevices`, `speechSynthesis`, `screen.orientation`,
//! `performance.memory` and `getGamepads`.
//!
//! Shapes, identities and error texts were compared with the local
//! Chromium 153 on loopback (see `docs/ROADMAP.md`, M4 status):
//! - `navigator.plugins`, `plugins[i]` and `mimeTypes[i]` are stable
//!   objects; `plugins[i][j]` is a new `MimeType` on every access; every
//!   `enabledPlugin` is `plugins[0]`.
//! - `navigator.userAgentData` is a new object on every access, and so is
//!   its frozen `brands` array.
//! - `permissions.query` returns a new `PermissionStatus` each time; its
//!   `name` is Chrome's internal one (`camera` → `video_capture`).
//! - `getBattery()` returns the same promise every time.
//! - Without camera/microphone permission `enumerateDevices()` lists at
//!   most one device per kind, with empty ids and labels.
//!
//! Values without a persona field use what Chrome 153 reports on a
//! desktop (battery full and charging, `jsHeapSizeLimit` 4,395,630,592).

use std::rc::Rc;

use lb_js::{Arg, CallCtx, HostError, HostRef, JsValue, ValueKind};
use lb_persona::PersonaProfile;

use super::{Dict, Obj, Runtime, Window, getter, illegal, method, need_args, this_obj, wf};
use crate::registry::{ImplKind, Registry};

// ---------------------------------------------------------------------------
// Objects
// ---------------------------------------------------------------------------

/// Host objects of this module (`Obj::Nav`).
pub(crate) enum NavObj {
    PluginArray,
    /// `navigator.plugins[i]`.
    Plugin(u8),
    MimeTypeArray,
    /// `mimeTypes[i]` or `plugins[p][i]`.
    MimeType(u8),
    UaData,
    Connection,
    Permissions,
    PermissionStatus {
        name: &'static str,
        state: String,
    },
    Notification(Box<NotificationData>),
    Battery,
    MediaDevices,
    DeviceInfo(Box<DeviceInfo>),
    Speech,
    Voice(u16),
    Orientation,
    /// WebRTC objects (`rtc`).
    Rtc(Box<super::rtc::RtcObj>),
    /// `performance.timing`.
    Timing,
    /// `performance.navigation`.
    Navigation,
}

impl NavObj {
    pub(crate) fn iface_name(&self) -> &'static str {
        match self {
            NavObj::PluginArray => "PluginArray",
            NavObj::Plugin(_) => "Plugin",
            NavObj::MimeTypeArray => "MimeTypeArray",
            NavObj::MimeType(_) => "MimeType",
            NavObj::UaData => "NavigatorUAData",
            NavObj::Connection => "NetworkInformation",
            NavObj::Permissions => "Permissions",
            NavObj::PermissionStatus { .. } => "PermissionStatus",
            NavObj::Notification(_) => "Notification",
            NavObj::Battery => "BatteryManager",
            NavObj::MediaDevices => "MediaDevices",
            NavObj::DeviceInfo(d) => {
                if d.kind == "audiooutput" {
                    "MediaDeviceInfo"
                } else {
                    "InputDeviceInfo"
                }
            }
            NavObj::Speech => "SpeechSynthesis",
            NavObj::Voice(_) => "SpeechSynthesisVoice",
            NavObj::Orientation => "ScreenOrientation",
            NavObj::Rtc(r) => r.iface_name(),
            NavObj::Timing => "PerformanceTiming",
            NavObj::Navigation => "PerformanceNavigation",
        }
    }
}

pub(crate) struct NotificationData {
    title: String,
    body: String,
    dir: String,
    lang: String,
    tag: String,
    icon: String,
    badge: String,
    image: String,
    silent: Option<bool>,
    require_interaction: bool,
    renotify: bool,
    timestamp: f64,
    data: Option<lb_js::Handle>,
}

pub(crate) struct DeviceInfo {
    kind: &'static str,
    id: String,
    label: String,
    group: String,
}

fn nav<R>(w: &Window, id: u32, f: impl FnOnce(&mut NavObj) -> Option<R>) -> Option<R> {
    w.with_obj(id, |o| match o {
        Obj::Nav(n) => f(n),
        _ => None,
    })
    .flatten()
}

fn new_nav(w: &Window, o: NavObj) -> Arg {
    w.obj_arg(w.new_obj(Obj::Nav(Box::new(o))))
}

/// A per-window singleton (same JS object on every access).
fn single(w: &Window, key: &'static str, idx: u32, make: impl FnOnce() -> NavObj) -> Arg {
    let id = w.cached_obj((idx, 0, key, String::new()), || Obj::Nav(Box::new(make())));
    w.obj_arg(id)
}

/// `this` as a `NavObj` matched by `f`, or `Illegal invocation`.
fn this_nav<R>(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    f: impl FnOnce(&mut NavObj) -> Option<R>,
) -> Result<R, HostError> {
    let id = this_obj(cx)?;
    nav(w, id, f).ok_or_else(illegal)
}

fn profile(w: &Window) -> Option<&PersonaProfile> {
    w.profile.as_deref()
}

/// A frozen array in the calling realm (`navigator.languages`,
/// `userAgentData.brands`).
fn frozen(cx: &mut CallCtx<'_, '_, '_>, w: &Window, json: String) -> Result<(), HostError> {
    let (parse, freeze) = {
        let h = w.helpers.borrow();
        (h.json_parse, h.freeze)
    };
    let (Some(parse), Some(freeze)) = (parse, freeze) else {
        return cx.ret(Arg::Json(json));
    };
    let arr = cx
        .try_call_handle(parse, Arg::Undefined, &[Arg::String(json)])
        .map_err(|e| HostError::Error(e.message))?;
    let _ = cx.try_call(freeze, Arg::Undefined, &[Arg::Handle(arr)]);
    let r = cx.ret(Arg::Handle(arr));
    cx.release(arr);
    r
}

fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

// ---------------------------------------------------------------------------
// Plugins and MIME types
// ---------------------------------------------------------------------------

/// Chrome's fixed plugin list (`pdfViewerEnabled`).
const PLUGINS: [&str; 5] = [
    "PDF Viewer",
    "Chrome PDF Viewer",
    "Chromium PDF Viewer",
    "Microsoft Edge PDF Viewer",
    "WebKit built-in PDF",
];
const MIME_TYPES: [&str; 2] = ["application/pdf", "text/pdf"];
const PDF_DESCRIPTION: &str = "Portable Document Format";

fn pdf_enabled(w: &Window) -> bool {
    profile(w).is_none_or(|p| p.browser.pdf_viewer_enabled)
}

fn plugin_count(w: &Window) -> u32 {
    if pdf_enabled(w) {
        PLUGINS.len() as u32
    } else {
        0
    }
}

fn mime_count(w: &Window) -> u32 {
    if pdf_enabled(w) {
        MIME_TYPES.len() as u32
    } else {
        0
    }
}

fn plugin_arg(w: &Window, i: u32) -> Option<Arg> {
    (i < plugin_count(w)).then(|| single(w, "nav:plugin", i, || NavObj::Plugin(i as u8)))
}

fn mime_arg(w: &Window, i: u32) -> Option<Arg> {
    (i < mime_count(w)).then(|| single(w, "nav:mime", i, || NavObj::MimeType(i as u8)))
}

/// `plugins[p][i]`: a new object on every access, like Chrome.
fn plugin_mime_arg(w: &Window, _plugin: u8, i: u32) -> Option<Arg> {
    (i < mime_count(w)).then(|| new_nav(w, NavObj::MimeType(i as u8)))
}

fn plugin_index(name: &str) -> Option<u32> {
    PLUGINS.iter().position(|p| *p == name).map(|i| i as u32)
}

fn mime_index(name: &str) -> Option<u32> {
    MIME_TYPES.iter().position(|p| *p == name).map(|i| i as u32)
}

/// The collection an interceptor runs on.
#[derive(Clone, Copy)]
enum Holder {
    PluginArray,
    MimeTypeArray,
    Plugin(u8),
}

/// Window and collection kind of an interceptor's holder (no borrow is
/// kept while the caller builds results).
fn holder(rt: &Runtime, cx: &mut lb_js::PropCtx<'_, '_, '_>) -> Option<(Rc<Window>, Holder)> {
    let w = rt.window(cx.realm())?;
    let h = cx.holder_host().filter(|h| h.kind == super::K_OBJ)?;
    let k = nav(&w, h.id, |o| match o {
        NavObj::PluginArray => Some(Holder::PluginArray),
        NavObj::MimeTypeArray => Some(Holder::MimeTypeArray),
        NavObj::Plugin(p) => Some(Holder::Plugin(*p)),
        _ => None,
    })?;
    Some((w, k))
}

fn holder_len(w: &Window, k: Holder) -> u32 {
    match k {
        Holder::PluginArray => plugin_count(w),
        Holder::MimeTypeArray | Holder::Plugin(_) => mime_count(w),
    }
}

fn holder_item(w: &Window, k: Holder, i: u32) -> Option<Arg> {
    match k {
        Holder::PluginArray => plugin_arg(w, i),
        Holder::MimeTypeArray => mime_arg(w, i),
        Holder::Plugin(p) => plugin_mime_arg(w, p, i),
    }
}

fn holder_index(k: Holder, name: &str) -> Option<u32> {
    match k {
        Holder::PluginArray => plugin_index(name),
        Holder::MimeTypeArray | Holder::Plugin(_) => mime_index(name),
    }
}

fn holder_names(w: &Window, k: Holder) -> Vec<String> {
    let n = holder_len(w, k) as usize;
    let names: &[&str] = match k {
        Holder::PluginArray => &PLUGINS,
        _ => &MIME_TYPES,
    };
    names[..n].iter().map(|s| (*s).to_owned()).collect()
}

fn register_plugins(reg: &mut Registry, rt: &Runtime) {
    // PluginArray
    getter(reg, rt, "PluginArray", "length", |cx, w| {
        this_nav(cx, w, |o| matches!(o, NavObj::PluginArray).then_some(()))?;
        cx.ret(plugin_count(w))
    });
    method(reg, rt, "PluginArray", "item", |cx, w| {
        this_nav(cx, w, |o| matches!(o, NavObj::PluginArray).then_some(()))?;
        need_args(cx, 1, "item", "PluginArray")?;
        let i = cx.arg_u32(0)?;
        cx.ret(plugin_arg(w, i).unwrap_or(Arg::Null))
    });
    method(reg, rt, "PluginArray", "namedItem", |cx, w| {
        this_nav(cx, w, |o| matches!(o, NavObj::PluginArray).then_some(()))?;
        need_args(cx, 1, "namedItem", "PluginArray")?;
        let name = cx.arg_string(0)?;
        let r = plugin_index(&name).and_then(|i| plugin_arg(w, i));
        cx.ret(r.unwrap_or(Arg::Null))
    });
    method(reg, rt, "PluginArray", "refresh", |cx, w| {
        this_nav(cx, w, |o| matches!(o, NavObj::PluginArray).then_some(()))?;
        Ok(())
    });

    // Plugin
    fn this_plugin(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<u8, HostError> {
        this_nav(cx, w, |o| match o {
            NavObj::Plugin(p) => Some(*p),
            _ => None,
        })
    }
    getter(reg, rt, "Plugin", "name", |cx, w| {
        let p = this_plugin(cx, w)?;
        cx.ret(PLUGINS[p as usize])
    });
    getter(reg, rt, "Plugin", "filename", |cx, w| {
        this_plugin(cx, w)?;
        cx.ret("internal-pdf-viewer")
    });
    getter(reg, rt, "Plugin", "description", |cx, w| {
        this_plugin(cx, w)?;
        cx.ret(PDF_DESCRIPTION)
    });
    getter(reg, rt, "Plugin", "length", |cx, w| {
        this_plugin(cx, w)?;
        cx.ret(mime_count(w))
    });
    method(reg, rt, "Plugin", "item", |cx, w| {
        let p = this_plugin(cx, w)?;
        need_args(cx, 1, "item", "Plugin")?;
        let i = cx.arg_u32(0)?;
        cx.ret(plugin_mime_arg(w, p, i).unwrap_or(Arg::Null))
    });
    method(reg, rt, "Plugin", "namedItem", |cx, w| {
        let p = this_plugin(cx, w)?;
        need_args(cx, 1, "namedItem", "Plugin")?;
        let name = cx.arg_string(0)?;
        let r = mime_index(&name).and_then(|i| plugin_mime_arg(w, p, i));
        cx.ret(r.unwrap_or(Arg::Null))
    });

    // MimeTypeArray
    getter(reg, rt, "MimeTypeArray", "length", |cx, w| {
        this_nav(cx, w, |o| matches!(o, NavObj::MimeTypeArray).then_some(()))?;
        cx.ret(mime_count(w))
    });
    method(reg, rt, "MimeTypeArray", "item", |cx, w| {
        this_nav(cx, w, |o| matches!(o, NavObj::MimeTypeArray).then_some(()))?;
        need_args(cx, 1, "item", "MimeTypeArray")?;
        let i = cx.arg_u32(0)?;
        cx.ret(mime_arg(w, i).unwrap_or(Arg::Null))
    });
    method(reg, rt, "MimeTypeArray", "namedItem", |cx, w| {
        this_nav(cx, w, |o| matches!(o, NavObj::MimeTypeArray).then_some(()))?;
        need_args(cx, 1, "namedItem", "MimeTypeArray")?;
        let name = cx.arg_string(0)?;
        let r = mime_index(&name).and_then(|i| mime_arg(w, i));
        cx.ret(r.unwrap_or(Arg::Null))
    });

    // MimeType
    fn this_mime(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<u8, HostError> {
        this_nav(cx, w, |o| match o {
            NavObj::MimeType(idx) => Some(*idx),
            _ => None,
        })
    }
    getter(reg, rt, "MimeType", "type", |cx, w| {
        let i = this_mime(cx, w)?;
        cx.ret(MIME_TYPES[i as usize])
    });
    getter(reg, rt, "MimeType", "suffixes", |cx, w| {
        this_mime(cx, w)?;
        cx.ret("pdf")
    });
    getter(reg, rt, "MimeType", "description", |cx, w| {
        this_mime(cx, w)?;
        cx.ret(PDF_DESCRIPTION)
    });
    getter(reg, rt, "MimeType", "enabledPlugin", |cx, w| {
        this_mime(cx, w)?;
        cx.ret(plugin_arg(w, 0).unwrap_or(Arg::Null))
    });

    // Indexed and named properties ([LegacyUnenumerableNamedProperties]).
    for iface in ["PluginArray", "Plugin", "MimeTypeArray"] {
        let r = rt.clone();
        reg.indexed_getter(
            iface,
            Rc::new(move |cx, i| Ok(holder(&r, cx).and_then(|(w, k)| holder_item(&w, k, i)))),
        );
        let r = rt.clone();
        reg.indexed_length(
            iface,
            Rc::new(move |cx| holder(&r, cx).map_or(0, |(w, k)| holder_len(&w, k))),
        );
        let r = rt.clone();
        reg.named_getter(
            iface,
            Rc::new(move |cx, name| {
                Ok(holder(&r, cx)
                    .and_then(|(w, k)| holder_index(k, name).and_then(|i| holder_item(&w, k, i))))
            }),
        );
        let r = rt.clone();
        reg.named_enumerator(
            iface,
            Rc::new(move |cx| holder(&r, cx).map_or_else(Vec::new, |(w, k)| holder_names(&w, k))),
        );
        reg.named_unenumerable(iface);
    }
}

// ---------------------------------------------------------------------------
// User-Agent Client Hints
// ---------------------------------------------------------------------------

/// Brands, full version list, platform and mobile from the persona; from
/// lb-persona's Chromium rule when the persona has no `client_hints`.
struct Hints {
    brands: Vec<(String, String)>,
    full: Vec<(String, String)>,
    full_version: String,
    platform: String,
    platform_version: String,
    architecture: String,
    bitness: String,
    model: String,
    mobile: bool,
    wow64: bool,
}

fn hints(w: &Window) -> Hints {
    let ua = w.persona.user_agent.clone();
    let major = lb_persona::uach::ua_major(&ua, "Chrome/").unwrap_or(153);
    let pairs = |v: &[lb_persona::BrandVersion]| -> Vec<(String, String)> {
        v.iter()
            .map(|b| (b.brand.clone(), b.version.clone()))
            .collect()
    };
    let p = profile(w);
    match p.and_then(|p| p.client_hints.as_ref()) {
        Some(h) => {
            let full_version = h
                .full_version
                .clone()
                .or_else(|| p.map(|p| p.browser.version.clone()))
                .unwrap_or_else(|| format!("{major}.0.0.0"));
            Hints {
                brands: pairs(&h.brands),
                full: if h.full_version_list.is_empty() {
                    pairs(&lb_persona::uach::chrome_full_version_list(
                        major,
                        &full_version,
                    ))
                } else {
                    pairs(&h.full_version_list)
                },
                full_version,
                platform: h.platform.clone(),
                platform_version: h.platform_version.clone(),
                architecture: h.architecture.clone(),
                bitness: h.bitness.clone(),
                model: h.model.clone(),
                mobile: h.mobile,
                wow64: h.wow64,
            }
        }
        None => {
            let full_version = format!("{major}.0.0.0");
            let platform = if ua.contains("Windows") {
                "Windows"
            } else if ua.contains("Mac OS X") {
                "macOS"
            } else if ua.contains("Android") {
                "Android"
            } else if ua.contains("CrOS") {
                "Chrome OS"
            } else {
                "Linux"
            };
            Hints {
                brands: pairs(&lb_persona::uach::chrome_brands(major)),
                full: pairs(&lb_persona::uach::chrome_full_version_list(
                    major,
                    &full_version,
                )),
                full_version,
                platform: platform.into(),
                platform_version: String::new(),
                architecture: "x86".into(),
                bitness: "64".into(),
                model: String::new(),
                mobile: ua.contains("Mobile"),
                wow64: false,
            }
        }
    }
}

fn brands_json(b: &[(String, String)]) -> String {
    let items: Vec<String> = b
        .iter()
        .map(|(n, v)| format!("{{\"brand\":{},\"version\":{}}}", json_str(n), json_str(v)))
        .collect();
    format!("[{}]", items.join(","))
}

/// Read a `sequence<DOMString>` argument (array-likes by `length`).
fn string_sequence(
    cx: &mut CallCtx<'_, '_, '_>,
    i: usize,
    op: &str,
    iface: &str,
) -> Result<Vec<String>, HostError> {
    let seq_err = || {
        HostError::type_error(format!(
            "Failed to execute '{op}' on '{iface}': The provided value cannot be converted to a sequence."
        ))
    };
    if !matches!(cx.arg_kind(i), ValueKind::Object | ValueKind::Function) {
        return Err(seq_err());
    }
    let h = cx.arg_handle(i);
    let len_h = cx.get_property(h, "length")?;
    let len = match cx.handle_value(len_h) {
        JsValue::Number(n) if n.is_finite() && n >= 0.0 => n as usize,
        _ => 0,
    };
    cx.release(len_h);
    let mut out = Vec::with_capacity(len.min(256));
    for k in 0..len.min(1 << 12) {
        let v = cx.get_property(h, &k.to_string())?;
        let s = cx.handle_string(v);
        cx.release(v);
        out.push(s?);
    }
    Ok(out)
}

fn register_ua_data(reg: &mut Registry, rt: &Runtime) {
    fn check(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<(), HostError> {
        this_nav(cx, w, |o| matches!(o, NavObj::UaData).then_some(()))
    }
    getter(reg, rt, "NavigatorUAData", "brands", |cx, w| {
        check(cx, w)?;
        frozen(cx, w, brands_json(&hints(w).brands))
    });
    getter(reg, rt, "NavigatorUAData", "mobile", |cx, w| {
        check(cx, w)?;
        cx.ret(hints(w).mobile)
    });
    getter(reg, rt, "NavigatorUAData", "platform", |cx, w| {
        check(cx, w)?;
        cx.ret(hints(w).platform)
    });
    method(reg, rt, "NavigatorUAData", "toJSON", |cx, w| {
        check(cx, w)?;
        let h = hints(w);
        cx.ret(Arg::Json(format!(
            "{{\"brands\":{},\"mobile\":{},\"platform\":{}}}",
            brands_json(&h.brands),
            h.mobile,
            json_str(&h.platform)
        )))
    });
    method(
        reg,
        rt,
        "NavigatorUAData",
        "getHighEntropyValues",
        |cx, w| {
            check(cx, w)?;
            need_args(cx, 1, "getHighEntropyValues", "NavigatorUAData")?;
            let r = cx.ret_new_promise()?;
            let wanted = match string_sequence(cx, 0, "getHighEntropyValues", "NavigatorUAData") {
                Ok(v) => v,
                Err(e) => return cx.reject_promise(r, e),
            };
            let h = hints(w);
            let has = |k: &str| wanted.iter().any(|x| x == k);
            // Keys in Chrome's (alphabetical) order; low-entropy ones always.
            let mut fields: Vec<String> = Vec::new();
            if has("architecture") {
                fields.push(format!("\"architecture\":{}", json_str(&h.architecture)));
            }
            if has("bitness") {
                fields.push(format!("\"bitness\":{}", json_str(&h.bitness)));
            }
            fields.push(format!("\"brands\":{}", brands_json(&h.brands)));
            if has("formFactors") {
                let ff = if h.mobile { "Mobile" } else { "Desktop" };
                fields.push(format!("\"formFactors\":[\"{ff}\"]"));
            }
            if has("fullVersionList") {
                fields.push(format!("\"fullVersionList\":{}", brands_json(&h.full)));
            }
            fields.push(format!("\"mobile\":{}", h.mobile));
            if has("model") {
                fields.push(format!("\"model\":{}", json_str(&h.model)));
            }
            fields.push(format!("\"platform\":{}", json_str(&h.platform)));
            if has("platformVersion") {
                fields.push(format!(
                    "\"platformVersion\":{}",
                    json_str(&h.platform_version)
                ));
            }
            if has("uaFullVersion") {
                fields.push(format!("\"uaFullVersion\":{}", json_str(&h.full_version)));
            }
            if has("wow64") {
                fields.push(format!("\"wow64\":{}", h.wow64));
            }
            cx.resolve_promise(r, Arg::Json(format!("{{{}}}", fields.join(","))))
        },
    );
}

// ---------------------------------------------------------------------------
// NetworkInformation
// ---------------------------------------------------------------------------

/// Chrome's per-host noise on `rtt` / `downlink` (0.90 to 1.10, keyed on
/// the host and a per-profile salt; here the salt comes from the persona's
/// canvas seed so a persona always gives the same values).
fn net_multiplier(w: &Window) -> f64 {
    let url = w.url();
    let Some(host) = url.host_str() else {
        return 1.0;
    };
    let salt = profile(w)
        .and_then(|p| {
            lb_persona::profile::FingerprintSeeds::bytes(&p.fingerprint_seeds.canvas_seed)
        })
        .map_or(1, |b| u32::from(b[0] % 20) + 1);
    let mut hash: u32 = 0x811c_9dc5;
    for b in host.bytes() {
        hash = (hash ^ u32::from(b)).wrapping_mul(0x0100_0193);
    }
    0.9 + f64::from(hash.wrapping_add(salt) % 21) * 0.01
}

/// `connection.rtt`: noised, capped at 3 s, rounded to 50 ms.
fn conn_rtt(w: &Window) -> u32 {
    let rtt = profile(w).map_or(100, |p| p.network.rtt);
    let ms = (f64::from(rtt) * net_multiplier(w)).min(3000.0);
    ((ms / 50.0).round() * 50.0) as u32
}

/// `connection.downlink` in Mbps: noised, capped at 10, rounded down to
/// 50 kbps.
fn conn_downlink(w: &Window) -> f64 {
    let mbps = profile(w).map_or(10.0, |p| p.network.downlink);
    let kbps = (mbps * 1000.0 * net_multiplier(w)).min(10_000.0);
    ((kbps as u64 / 50) * 50) as f64 / 1000.0
}

fn effective_type(w: &Window) -> &'static str {
    match profile(w).map(|p| p.network.connection_type.as_str()) {
        Some("slow-2g") => "slow-2g",
        Some("2g") => "2g",
        Some("3g") => "3g",
        _ => "4g",
    }
}

fn register_connection(reg: &mut Registry, rt: &Runtime) {
    fn check(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<(), HostError> {
        this_nav(cx, w, |o| matches!(o, NavObj::Connection).then_some(()))
    }
    getter(reg, rt, "NetworkInformation", "effectiveType", |cx, w| {
        check(cx, w)?;
        cx.ret(effective_type(w))
    });
    getter(reg, rt, "NetworkInformation", "rtt", |cx, w| {
        check(cx, w)?;
        cx.ret(conn_rtt(w))
    });
    getter(reg, rt, "NetworkInformation", "downlink", |cx, w| {
        check(cx, w)?;
        cx.ret(conn_downlink(w))
    });
    getter(reg, rt, "NetworkInformation", "saveData", |cx, w| {
        check(cx, w)?;
        cx.ret(profile(w).is_some_and(|p| p.network.save_data))
    });
}

// ---------------------------------------------------------------------------
// Permissions and Notification
// ---------------------------------------------------------------------------

/// How Chrome 153 answers `permissions.query({name})` on a desktop page.
enum PermRule {
    /// Status name and default state; the persona may override the state.
    State(&'static str, &'static str),
    /// TypeError with this text.
    Disabled(&'static str),
}

fn perm_rule(name: &str) -> Option<PermRule> {
    use PermRule::*;
    Some(match name {
        "geolocation" => State("geolocation", "prompt"),
        "notifications" | "push" => State("notifications", "prompt"),
        "midi" => State("midi", "prompt"),
        "camera" => State("video_capture", "prompt"),
        "microphone" => State("audio_capture", "prompt"),
        "background-fetch" => State("background_fetch", "granted"),
        "background-sync" => State("background_sync", "granted"),
        "persistent-storage" => State("durable_storage", "prompt"),
        "ambient-light-sensor" => Disabled("GenericSensorExtraClasses flag is not enabled."),
        "accelerometer" | "gyroscope" | "magnetometer" => State("sensors", "granted"),
        "screen-wake-lock" => State("screen_wake_lock", "granted"),
        "nfc" => Disabled("Web NFC is not enabled."),
        "display-capture" => State("display_capture", "prompt"),
        "clipboard-read" => State("clipboard_read", "prompt"),
        "clipboard-write" => State("clipboard_write", "granted"),
        "payment-handler" => State("payment_handler", "granted"),
        "idle-detection" => State("idle_detection", "prompt"),
        "periodic-background-sync" => State("periodic_background_sync", "denied"),
        "system-wake-lock" => Disabled("System Wake Lock is not enabled."),
        "storage-access" => State("storage-access", "granted"),
        "window-management" => State("window-management", "prompt"),
        "local-fonts" => State("local_fonts", "prompt"),
        "top-level-storage-access" => State("top-level-storage-access", "prompt"),
        "captured-surface-control" => State("captured-surface-control", "prompt"),
        "speaker-selection" => Disabled("The Speaker Selection API is not enabled."),
        "keyboard-lock" => State("keyboard-lock", "granted"),
        "pointer-lock" => State("pointer-lock", "granted"),
        "fullscreen" => State("fullscreen", "denied"),
        "web-app-installation" => Disabled("The Web App Install API is not enabled."),
        "local-network-access" => State("local-network-access", "prompt"),
        "local-network" => State("local-network", "prompt"),
        "loopback-network" => State("loopback-network", "prompt"),
        "geolocation-approximate" => {
            Disabled("Permission API support for approximate geolocation is not enabled.")
        }
        _ => return None,
    })
}

/// The persona's state for a permission (`granted`, `denied`, `prompt`).
fn persona_state(w: &Window, name: &str, default: &str) -> String {
    let key = if name == "push" {
        "notifications"
    } else {
        name
    };
    profile(w)
        .and_then(|p| p.permissions.get(key))
        .filter(|s| matches!(s.as_str(), "granted" | "denied" | "prompt"))
        .cloned()
        .unwrap_or_else(|| default.to_owned())
}

/// `Notification.permission` from the persona's `notifications` state.
fn notification_permission(w: &Window) -> &'static str {
    match persona_state(w, "notifications", "prompt").as_str() {
        "granted" => "granted",
        "denied" => "denied",
        _ => "default",
    }
}

fn register_permissions(reg: &mut Registry, rt: &Runtime) {
    method(reg, rt, "Permissions", "query", |cx, w| {
        this_nav(cx, w, |o| matches!(o, NavObj::Permissions).then_some(()))?;
        let r = cx.ret_new_promise()?;
        let pre = "Failed to execute 'query' on 'Permissions': ";
        if cx.is_empty() {
            return cx.reject_promise(
                r,
                HostError::type_error(format!("{pre}1 argument required, but only 0 present.")),
            );
        }
        if !matches!(cx.arg_kind(0), ValueKind::Object | ValueKind::Function) {
            return cx.reject_promise(
                r,
                HostError::type_error(format!("{pre}parameter 1 is not of type 'object'.")),
            );
        }
        let d = Dict::arg(cx, 0);
        let name = d.string(cx, "name");
        let user_visible = d.bool(cx, "userVisibleOnly");
        let allow_no_gesture = d.bool(cx, "allowWithoutGesture");
        let origin = d.string(cx, "requestedOrigin");
        d.release(cx);
        let read = "Failed to read the 'name' property from 'PermissionDescriptor': ";
        let name = match name {
            Ok(Some(n)) => n,
            Ok(None) => {
                return cx.reject_promise(
                    r,
                    HostError::type_error(format!("{pre}{read}Required member is undefined.")),
                );
            }
            Err(e) => return cx.reject_promise(r, e),
        };
        let Some(rule) = perm_rule(&name) else {
            return cx.reject_promise(
                r,
                HostError::type_error(format!(
                    "{pre}{read}The provided value '{name}' is not a valid enum value of type PermissionName."
                )),
            );
        };
        let (status, default) = match rule {
            PermRule::Disabled(msg) => {
                return cx.reject_promise(r, HostError::type_error(format!("{pre}{msg}")));
            }
            PermRule::State(s, d) => (s, d),
        };
        if name == "push" && !user_visible.unwrap_or(false) {
            return cx.reject_promise(
                r,
                w.rt.err(
                    "NotSupportedError",
                    format!(
                        "{pre}Push Permission without userVisibleOnly:true isn't supported yet."
                    ),
                ),
            );
        }
        if name == "fullscreen" && !allow_no_gesture.unwrap_or(false) {
            return cx.reject_promise(
                r,
                HostError::type_error(format!(
                    "{pre}Fullscreen Permission only supports allowWithoutGesture:true."
                )),
            );
        }
        if name == "top-level-storage-access"
            && !origin
                .ok()
                .flatten()
                .is_some_and(|o| url::Url::parse(&o).is_ok_and(|u| u.scheme().starts_with("http")))
        {
            return cx.reject_promise(
                r,
                HostError::type_error(format!("{pre}The requested origin is invalid.")),
            );
        }
        let state = persona_state(w, &name, default);
        let status = new_nav(
            w,
            NavObj::PermissionStatus {
                name: status,
                state,
            },
        );
        cx.resolve_promise(r, status)
    });
    getter(reg, rt, "PermissionStatus", "name", |cx, w| {
        let n = this_nav(cx, w, |o| match o {
            NavObj::PermissionStatus { name, .. } => Some(*name),
            _ => None,
        })?;
        cx.ret(n)
    });
    getter(reg, rt, "PermissionStatus", "state", |cx, w| {
        let s = this_nav(cx, w, |o| match o {
            NavObj::PermissionStatus { state, .. } => Some(state.clone()),
            _ => None,
        })?;
        cx.ret(s)
    });

    // Notification
    reg.register(
        "Notification",
        "permission",
        ImplKind::StaticGetter,
        wf(rt, |cx, w| cx.ret(notification_permission(w))),
    );
    reg.register(
        "Notification",
        "maxActions",
        ImplKind::StaticGetter,
        wf(rt, |cx, _w| cx.ret(2)),
    );
    // A real desktop shows a prompt nobody answers: the promise (and the
    // legacy callback) stay pending unless the persona already decided.
    reg.register(
        "Notification",
        "requestPermission",
        ImplKind::StaticMethod,
        wf(rt, |cx, w| {
            let r = cx.ret_new_promise()?;
            let p = notification_permission(w);
            if p == "default" {
                return Ok(());
            }
            if matches!(cx.arg_kind(0), ValueKind::Function) {
                let f = cx.arg_handle(0);
                let _ = cx.try_call(f, Arg::Undefined, &[Arg::String(p.into())]);
            }
            cx.resolve_promise(r, Arg::String(p.into()))
        }),
    );
    super::ctor(reg, rt, "Notification", |cx, w| {
        if !cx.is_construct_call() {
            return Err(HostError::type_error(
                "Failed to construct 'Notification': Please use the 'new' operator, this DOM object constructor cannot be called as a function.",
            ));
        }
        if cx.is_empty() {
            return Err(HostError::type_error(
                "Failed to construct 'Notification': 1 argument required, but only 0 present.",
            ));
        }
        let title = cx.arg_string(0)?;
        let d = Dict::arg(cx, 1);
        let s = |cx: &mut CallCtx<'_, '_, '_>, k: &str| -> Result<String, HostError> {
            Ok(d.string(cx, k)?.unwrap_or_default())
        };
        let body = s(cx, "body")?;
        let dir = d.string(cx, "dir")?.unwrap_or_else(|| "auto".into());
        let lang = s(cx, "lang")?;
        let tag = s(cx, "tag")?;
        let resolve = |w: &Window, u: String| -> String {
            if u.is_empty() {
                u
            } else {
                w.resolve_url(&u).map_or(u, |x| x.to_string())
            }
        };
        let icon = resolve(w, s(cx, "icon")?);
        let badge = resolve(w, s(cx, "badge")?);
        let image = resolve(w, s(cx, "image")?);
        let silent = match d.get(cx, "silent")? {
            Some(h) => {
                let v = super::truthy(&cx.handle_value(h));
                cx.release(h);
                Some(v)
            }
            None => None,
        };
        let require_interaction = d.bool(cx, "requireInteraction")?;
        let renotify = d.bool(cx, "renotify")?;
        let timestamp = d
            .number(cx, "timestamp")?
            .unwrap_or_else(|| (w.clock.time_origin + w.clock.now()).floor());
        let data = d.get(cx, "data")?;
        d.release(cx);
        if renotify && tag.is_empty() {
            return Err(HostError::type_error(
                "Failed to construct 'Notification': Notifications which set the renotify flag must specify a non-empty tag.",
            ));
        }
        let id = w.new_obj(Obj::Nav(Box::new(NavObj::Notification(Box::new(
            NotificationData {
                title,
                body,
                dir,
                lang,
                tag,
                icon,
                badge,
                image,
                silent,
                require_interaction,
                renotify,
                timestamp,
                data,
            },
        )))));
        cx.init_this(HostRef::new(super::K_OBJ, id))?;
        // Without permission Chrome fires `error` at the new notification.
        if notification_permission(w) != "granted" {
            let target = HostRef::new(super::K_OBJ, id);
            w.queue_task(
                super::TaskSource::DomManipulation,
                Box::new(move |js, w| {
                    super::fire_event(js, w, target, "error", false, false);
                }),
            );
        }
        Ok(())
    });
    macro_rules! notif {
        ($name:literal, |$n:ident| $e:expr) => {
            getter(reg, rt, "Notification", $name, |cx, w| {
                let v: Arg = this_nav(cx, w, |o| match o {
                    NavObj::Notification($n) => Some(Arg::from($e)),
                    _ => None,
                })?;
                cx.ret(v)
            });
        };
    }
    notif!("title", |n| n.title.clone());
    notif!("body", |n| n.body.clone());
    notif!("dir", |n| n.dir.clone());
    notif!("lang", |n| n.lang.clone());
    notif!("tag", |n| n.tag.clone());
    notif!("icon", |n| n.icon.clone());
    notif!("badge", |n| n.badge.clone());
    notif!("image", |n| n.image.clone());
    notif!("requireInteraction", |n| n.require_interaction);
    notif!("renotify", |n| n.renotify);
    notif!("timestamp", |n| n.timestamp);
    notif!("silent", |n| n.silent.unwrap_or(false));
    notif!("data", |n| n.data.map_or(Arg::Null, Arg::Handle));
    notif!("actions", |_n| Arg::Array(Vec::new()));
    notif!("vibrate", |_n| Arg::Array(Vec::new()));
    method(reg, rt, "Notification", "close", |cx, w| {
        this_nav(cx, w, |o| {
            matches!(o, NavObj::Notification(_)).then_some(())
        })?;
        Ok(())
    });
}

// ---------------------------------------------------------------------------
// Battery
// ---------------------------------------------------------------------------

fn register_battery(reg: &mut Registry, rt: &Runtime) {
    fn info(
        cx: &mut CallCtx<'_, '_, '_>,
        w: &Window,
    ) -> Result<lb_persona::BatteryInfo, HostError> {
        this_nav(cx, w, |o| matches!(o, NavObj::Battery).then_some(()))?;
        Ok(profile(w).map_or_else(lb_persona::BatteryInfo::desktop, |p| p.battery_info()))
    }
    getter(reg, rt, "BatteryManager", "charging", |cx, w| {
        let b = info(cx, w)?;
        cx.ret(b.charging)
    });
    getter(reg, rt, "BatteryManager", "level", |cx, w| {
        let b = info(cx, w)?;
        cx.ret(b.level.clamp(0.0, 1.0))
    });
    getter(reg, rt, "BatteryManager", "chargingTime", |cx, w| {
        let b = info(cx, w)?;
        let full = b.charging && b.level >= 1.0;
        cx.ret(match b.charging_time {
            Some(t) => t,
            None if full => 0.0,
            None => f64::INFINITY,
        })
    });
    getter(reg, rt, "BatteryManager", "dischargingTime", |cx, w| {
        let b = info(cx, w)?;
        cx.ret(match b.discharging_time {
            Some(t) if !b.charging => t,
            _ => f64::INFINITY,
        })
    });
    // Same promise on every call (per realm), resolved with the one
    // BatteryManager.
    method(reg, rt, "Navigator", "getBattery", |cx, w| {
        this_obj(cx)?;
        let realm = cx.realm();
        let cached = w.nav_state.borrow().battery.get(&realm).copied();
        if let Some(p) = cached {
            return cx.ret(Arg::Handle(p));
        }
        let mgr = single(w, "nav:battery", 0, || NavObj::Battery);
        let resolve = w.nav_state.borrow().promise_resolve.get(&realm).copied();
        let Some((ctor, resolve)) = resolve else {
            let r = cx.ret_new_promise()?;
            return cx.resolve_promise(r, mgr);
        };
        let p = cx
            .try_call_handle(resolve, Arg::Handle(ctor), &[mgr])
            .map_err(|e| HostError::Error(e.message))?;
        w.nav_state.borrow_mut().battery.insert(realm, p);
        cx.ret(Arg::Handle(p))
    });
}

// ---------------------------------------------------------------------------
// Media devices
// ---------------------------------------------------------------------------

/// A 64-hex-digit id (Chrome's HMAC-SHA256 device ids), stable for the
/// persona, origin and `what`.
fn device_hash(w: &Window, what: &str) -> String {
    let seed = profile(w)
        .and_then(|p| lb_persona::profile::FingerprintSeeds::bytes(&p.fingerprint_seeds.audio_seed))
        .unwrap_or([7; 16]);
    let origin = w.url().origin().ascii_serialization();
    let mut key = seed;
    for (i, b) in origin.bytes().chain(what.bytes()).enumerate() {
        key[i % 16] = key[i % 16].rotate_left(3) ^ b;
    }
    let mut rng = lb_persona::sfc32::Sfc32::new(key);
    (0..8).map(|_| format!("{:08x}", rng.next_u32())).collect()
}

fn devices(w: &Window) -> Vec<DeviceInfo> {
    let md = profile(w)
        .and_then(|p| p.media_devices.clone())
        .unwrap_or_default();
    let (mics, cams, speakers) = md.counts();
    let granted = |k: &str| persona_state(w, k, "prompt") == "granted";
    let (mic_ok, cam_ok) = (granted("microphone"), granted("camera"));
    let mut out = Vec::new();
    let mut add = |kind: &'static str, n: u32, label: &Option<String>, ok: bool| {
        if n == 0 {
            return;
        }
        if !ok {
            // One entry per kind, nothing identifying.
            out.push(DeviceInfo {
                kind,
                id: String::new(),
                label: String::new(),
                group: String::new(),
            });
            return;
        }
        let base = label.clone().unwrap_or_default();
        let audio = kind != "videoinput";
        let mut push = |id: String, label: String, group: usize| {
            let group = device_hash(w, &format!("group{group}"));
            out.push(DeviceInfo {
                kind,
                id,
                label,
                group,
            });
        };
        if audio {
            push("default".into(), format!("Default - {base}"), 0);
        }
        for i in 0..n {
            let label = if i == 0 {
                base.clone()
            } else {
                format!("{base} ({})", i + 1)
            };
            push(device_hash(w, &format!("{kind}{i}")), label, i as usize);
        }
    };
    add("audioinput", mics, &md.mic_label, mic_ok);
    add("videoinput", cams, &md.webcam_label, cam_ok);
    // Output labels follow microphone permission, as in Chrome.
    add("audiooutput", speakers, &md.speaker_label, mic_ok);
    out
}

const SUPPORTED_CONSTRAINTS: &[&str] = &[
    "aspectRatio",
    "autoGainControl",
    "brightness",
    "channelCount",
    "colorTemperature",
    "contrast",
    "deviceId",
    "displaySurface",
    "echoCancellation",
    "exposureCompensation",
    "exposureMode",
    "exposureTime",
    "facingMode",
    "focusDistance",
    "focusMode",
    "frameRate",
    "groupId",
    "height",
    "iso",
    "latency",
    "noiseSuppression",
    "pan",
    "pointsOfInterest",
    "resizeMode",
    "restrictOwnAudio",
    "sampleRate",
    "sampleSize",
    "saturation",
    "sharpness",
    "suppressLocalAudioPlayback",
    "tilt",
    "torch",
    "voiceIsolation",
    "whiteBalanceMode",
    "width",
    "zoom",
];

fn register_media(reg: &mut Registry, rt: &Runtime) {
    method(reg, rt, "MediaDevices", "enumerateDevices", |cx, w| {
        this_nav(cx, w, |o| matches!(o, NavObj::MediaDevices).then_some(()))?;
        let r = cx.ret_new_promise()?;
        let list: Vec<Arg> = devices(w)
            .into_iter()
            .map(|d| new_nav(w, NavObj::DeviceInfo(Box::new(d))))
            .collect();
        cx.resolve_promise(r, Arg::Array(list))
    });
    method(
        reg,
        rt,
        "MediaDevices",
        "getSupportedConstraints",
        |cx, w| {
            this_nav(cx, w, |o| matches!(o, NavObj::MediaDevices).then_some(()))?;
            let body: Vec<String> = SUPPORTED_CONSTRAINTS
                .iter()
                .map(|k| format!("\"{k}\":true"))
                .collect();
            cx.ret(Arg::Json(format!("{{{}}}", body.join(","))))
        },
    );
    // No capture devices behind the persona: a denied permission rejects
    // like Chrome; otherwise the prompt is left unanswered.
    for (name, is_display) in [("getUserMedia", false), ("getDisplayMedia", true)] {
        method(reg, rt, "MediaDevices", name, move |cx, w| {
            this_nav(cx, w, |o| matches!(o, NavObj::MediaDevices).then_some(()))?;
            let r = cx.ret_new_promise()?;
            let d = Dict::arg(cx, 0);
            let video = d.get(cx, "video")?.is_some_and(|h| {
                let v = super::truthy(&cx.handle_value(h));
                cx.release(h);
                v
            });
            let audio = d.get(cx, "audio")?.is_some_and(|h| {
                let v = super::truthy(&cx.handle_value(h));
                cx.release(h);
                v
            });
            d.release(cx);
            if !is_display && !video && !audio {
                return cx.reject_promise(
                    r,
                    HostError::type_error(format!(
                        "Failed to execute '{name}' on 'MediaDevices': At least one of audio and video must be requested"
                    )),
                );
            }
            let state = |k| persona_state(w, k, "prompt");
            let denied = (video && state("camera") == "denied")
                || (audio && state("microphone") == "denied");
            if denied {
                return cx.reject_promise(r, w.rt.err("NotAllowedError", "Permission denied"));
            }
            Ok(())
        });
    }
    fn dev<R>(
        cx: &mut CallCtx<'_, '_, '_>,
        w: &Window,
        f: impl FnOnce(&DeviceInfo) -> R,
    ) -> Result<R, HostError> {
        this_nav(cx, w, |o| match o {
            NavObj::DeviceInfo(d) => Some(f(d)),
            _ => None,
        })
    }
    getter(reg, rt, "MediaDeviceInfo", "deviceId", |cx, w| {
        let v = dev(cx, w, |d| d.id.clone())?;
        cx.ret(v)
    });
    getter(reg, rt, "MediaDeviceInfo", "kind", |cx, w| {
        let v = dev(cx, w, |d| d.kind)?;
        cx.ret(v)
    });
    getter(reg, rt, "MediaDeviceInfo", "label", |cx, w| {
        let v = dev(cx, w, |d| d.label.clone())?;
        cx.ret(v)
    });
    getter(reg, rt, "MediaDeviceInfo", "groupId", |cx, w| {
        let v = dev(cx, w, |d| d.group.clone())?;
        cx.ret(v)
    });
    method(reg, rt, "MediaDeviceInfo", "toJSON", |cx, w| {
        let v = dev(cx, w, |d| {
            format!(
                "{{\"deviceId\":{},\"kind\":\"{}\",\"label\":{},\"groupId\":{}}}",
                json_str(&d.id),
                d.kind,
                json_str(&d.label),
                json_str(&d.group)
            )
        })?;
        cx.ret(Arg::Json(v))
    });
    method(reg, rt, "InputDeviceInfo", "getCapabilities", |cx, w| {
        let v = dev(cx, w, |d| {
            if d.id.is_empty() {
                "{}".to_owned()
            } else {
                format!(
                    "{{\"deviceId\":{},\"groupId\":{}}}",
                    json_str(&d.id),
                    json_str(&d.group)
                )
            }
        })?;
        cx.ret(Arg::Json(v))
    });
}

// ---------------------------------------------------------------------------
// Speech synthesis
// ---------------------------------------------------------------------------

fn voice_count(w: &Window) -> usize {
    profile(w).map_or(0, |p| p.speech_voices.len())
}

fn register_speech(reg: &mut Registry, rt: &Runtime) {
    fn check(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<(), HostError> {
        this_nav(cx, w, |o| matches!(o, NavObj::Speech).then_some(()))
    }
    method(reg, rt, "SpeechSynthesis", "getVoices", |cx, w| {
        check(cx, w)?;
        let list: Vec<Arg> = (0..voice_count(w).min(u16::MAX as usize))
            .map(|i| single(w, "nav:voice", i as u32, || NavObj::Voice(i as u16)))
            .collect();
        cx.ret(Arg::Array(list))
    });
    for name in ["pending", "speaking", "paused"] {
        getter(reg, rt, "SpeechSynthesis", name, |cx, w| {
            check(cx, w)?;
            cx.ret(false)
        });
    }
    for name in ["cancel", "pause", "resume"] {
        method(reg, rt, "SpeechSynthesis", name, |cx, w| {
            check(cx, w)?;
            Ok(())
        });
    }
    method(reg, rt, "SpeechSynthesis", "speak", |cx, w| {
        check(cx, w)?;
        need_args(cx, 1, "speak", "SpeechSynthesis")
    });
    fn voice(
        cx: &mut CallCtx<'_, '_, '_>,
        w: &Window,
    ) -> Result<lb_persona::profile::SpeechVoice, HostError> {
        let i = this_nav(cx, w, |o| match o {
            NavObj::Voice(i) => Some(*i as usize),
            _ => None,
        })?;
        profile(w)
            .and_then(|p| p.speech_voices.get(i).cloned())
            .ok_or_else(illegal)
    }
    getter(reg, rt, "SpeechSynthesisVoice", "voiceURI", |cx, w| {
        let v = voice(cx, w)?;
        cx.ret(v.name)
    });
    getter(reg, rt, "SpeechSynthesisVoice", "name", |cx, w| {
        let v = voice(cx, w)?;
        cx.ret(v.name)
    });
    getter(reg, rt, "SpeechSynthesisVoice", "lang", |cx, w| {
        let v = voice(cx, w)?;
        cx.ret(v.lang)
    });
    getter(reg, rt, "SpeechSynthesisVoice", "localService", |cx, w| {
        let v = voice(cx, w)?;
        cx.ret(v.local_service)
    });
    getter(reg, rt, "SpeechSynthesisVoice", "default", |cx, w| {
        let v = voice(cx, w)?;
        cx.ret(v.default)
    });
}

// ---------------------------------------------------------------------------
// Screen orientation, performance.memory
// ---------------------------------------------------------------------------

fn register_screen_extras(reg: &mut Registry, rt: &Runtime) {
    getter(reg, rt, "Screen", "orientation", |cx, w| {
        this_obj(cx)?;
        cx.ret(single(w, "nav:orientation", 0, || NavObj::Orientation))
    });
    fn check(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<(), HostError> {
        this_nav(cx, w, |o| matches!(o, NavObj::Orientation).then_some(()))
    }
    getter(reg, rt, "ScreenOrientation", "type", |cx, w| {
        check(cx, w)?;
        let s = super::window::screen_dims(w);
        cx.ret(if s.width >= s.height {
            "landscape-primary"
        } else {
            "portrait-primary"
        })
    });
    getter(reg, rt, "ScreenOrientation", "angle", |cx, w| {
        check(cx, w)?;
        cx.ret(0)
    });
    method(reg, rt, "ScreenOrientation", "lock", |cx, w| {
        check(cx, w)?;
        let r = cx.ret_new_promise()?;
        if cx.is_empty() {
            return cx.reject_promise(
                r,
                HostError::type_error(
                    "Failed to execute 'lock' on 'ScreenOrientation': 1 argument required, but only 0 present.",
                ),
            );
        }
        cx.reject_promise(
            r,
            w.rt.err(
                "NotSupportedError",
                "screen.orientation.lock() is not available on this device.",
            ),
        )
    });
    method(reg, rt, "ScreenOrientation", "unlock", |cx, w| {
        check(cx, w)?;
        Ok(())
    });

    // performance.memory: a new MemoryInfo on every access. MemoryInfo
    // is `[LegacyNoInterfaceObject]`, so it is not in the binding table:
    // each realm gets a prototype with native getters (`setup_realm`) and
    // instances carry their values in private fields.
    getter(reg, rt, "Performance", "memory", |cx, w| {
        this_obj(cx)?;
        let stats = cx.scope().get_heap_statistics();
        let limit = profile(w).map_or(4_395_630_592, |p| p.hardware.js_heap_size_limit());
        let realm = cx.realm();
        let mem = w.nav_state.borrow().memory.get(&realm).copied();
        // The engine's heap also holds the bindings of every interface,
        // other realms and isolated worlds. Report what this page added on
        // top of a blank Chrome 153 page (about 1.16 MB used, 1.57 MB
        // total).
        let (base_used, base_total) = mem.map_or((0.0, 0.0), |m| (m.base_used, m.base_total));
        let used = 1_157_700.0 + (stats.used_heap_size() as f64 - base_used).max(0.0);
        let total =
            (1_572_912.0 + (stats.total_heap_size() as f64 - base_total).max(0.0)).max(used);
        let vals = [
            Arg::Number(total.round()),
            Arg::Number(used.round()),
            Arg::Number(limit as f64),
        ];
        let Some(m) = mem else {
            let [t, u, l] = vals.map(|v| match v {
                Arg::Number(n) => n,
                _ => 0.0,
            });
            return cx.ret(Arg::Json(format!(
                "{{\"totalJSHeapSize\":{t},\"usedJSHeapSize\":{u},\"jsHeapSizeLimit\":{l}}}"
            )));
        };
        let obj = cx
            .try_call_handle(m.create, Arg::Undefined, &[Arg::Handle(m.proto)])
            .map_err(|e| HostError::Error(e.message))?;
        let _ = cx.try_call(m.mark, Arg::Handle(obj), &vals);
        let r = cx.ret(Arg::Handle(obj));
        cx.release(obj);
        r
    });
}

/// `performance.timing` fields in Chrome's order.
const TIMING_FIELDS: [&str; 21] = [
    "navigationStart",
    "unloadEventStart",
    "unloadEventEnd",
    "redirectStart",
    "redirectEnd",
    "fetchStart",
    "domainLookupStart",
    "domainLookupEnd",
    "connectStart",
    "connectEnd",
    "secureConnectionStart",
    "requestStart",
    "responseStart",
    "responseEnd",
    "domLoading",
    "domInteractive",
    "domContentLoadedEventStart",
    "domContentLoadedEventEnd",
    "domComplete",
    "loadEventStart",
    "loadEventEnd",
];

/// Legacy `performance.timing` value of `field` (ms since the epoch, 0
/// when not reached). The window starts at commit; the fetch before it
/// is placed in the first few milliseconds (a fast local load).
fn timing_value(w: &Window, field: &str) -> f64 {
    let nav = w.clock.time_origin.floor();
    let li = w.load_info.borrow().clone();
    let at = |ms: Option<f64>| ms.map_or(0.0, |m| (w.clock.time_origin + m).floor());
    let https = w.url().scheme() == "https";
    match field {
        "navigationStart" => nav,
        "fetchStart" | "domainLookupStart" | "domainLookupEnd" | "connectStart" => nav + 1.0,
        "connectEnd" => nav + 1.0,
        "secureConnectionStart" if https => nav + 1.0,
        "requestStart" => nav + 2.0,
        "responseStart" => nav + 3.0,
        "responseEnd" | "domLoading" => nav + 4.0,
        "domInteractive" | "domContentLoadedEventStart" | "domContentLoadedEventEnd" => {
            at(li.dcl_ms)
        }
        "domComplete" | "loadEventStart" | "loadEventEnd" => at(li.load_ms),
        _ => 0.0,
    }
}

fn register_timing(reg: &mut Registry, rt: &Runtime) {
    getter(reg, rt, "Performance", "timing", |cx, w| {
        this_obj(cx)?;
        cx.ret(single(w, "nav:timing", 0, || NavObj::Timing))
    });
    getter(reg, rt, "Performance", "navigation", |cx, w| {
        this_obj(cx)?;
        cx.ret(single(w, "nav:navigation", 0, || NavObj::Navigation))
    });
    for field in TIMING_FIELDS {
        getter(reg, rt, "PerformanceTiming", field, move |cx, w| {
            this_nav(cx, w, |o| matches!(o, NavObj::Timing).then_some(()))?;
            cx.ret(timing_value(w, field))
        });
    }
    method(reg, rt, "PerformanceTiming", "toJSON", |cx, w| {
        this_nav(cx, w, |o| matches!(o, NavObj::Timing).then_some(()))?;
        let body: Vec<String> = TIMING_FIELDS
            .iter()
            .map(|f| format!("\"{f}\":{}", timing_value(w, f)))
            .collect();
        cx.ret(Arg::Json(format!("{{{}}}", body.join(","))))
    });
    let nav_type = |w: &Window| -> u32 {
        match w.load_info.borrow().navigation_type.as_deref() {
            Some("Reload") => 1,
            Some("BackForward") => 2,
            _ => 0,
        }
    };
    getter(reg, rt, "PerformanceNavigation", "type", move |cx, w| {
        this_nav(cx, w, |o| matches!(o, NavObj::Navigation).then_some(()))?;
        cx.ret(nav_type(w))
    });
    getter(
        reg,
        rt,
        "PerformanceNavigation",
        "redirectCount",
        |cx, w| {
            this_nav(cx, w, |o| matches!(o, NavObj::Navigation).then_some(()))?;
            cx.ret(0)
        },
    );
    method(reg, rt, "PerformanceNavigation", "toJSON", move |cx, w| {
        this_nav(cx, w, |o| matches!(o, NavObj::Navigation).then_some(()))?;
        cx.ret(Arg::Json(format!(
            "{{\"type\":{},\"redirectCount\":0}}",
            nav_type(w)
        )))
    });
}

/// Private keys of a MemoryInfo instance's values.
const MEM_KEYS: [&str; 3] = [
    "MemoryInfo#totalJSHeapSize",
    "MemoryInfo#usedJSHeapSize",
    "MemoryInfo#jsHeapSizeLimit",
];

/// Per-realm MemoryInfo machinery.
#[derive(Clone, Copy)]
pub(crate) struct MemoryProto {
    proto: lb_js::Handle,
    /// `Object.create`.
    create: lb_js::Handle,
    /// Native function storing the three values on `this`.
    mark: lb_js::Handle,
    /// Engine heap use when the realm was created.
    base_used: f64,
    base_total: f64,
}

fn setup_memory(
    engine: &mut dyn lb_js::RealmOps,
    realm: lb_js::RealmId,
) -> Result<MemoryProto, lb_js::JsError> {
    let mark = engine.new_function(
        realm,
        "",
        3,
        crate::registry::host_fn(|cx| {
            for (i, k) in MEM_KEYS.iter().enumerate() {
                let v = cx.arg_f64(i)?;
                cx.set_this_private(k, v)?;
            }
            Ok(())
        }),
    )?;
    let mut getters = Vec::new();
    for k in MEM_KEYS {
        let name = k.trim_start_matches("MemoryInfo#");
        let f = engine.new_function(
            realm,
            &format!("get {name}"),
            0,
            crate::registry::host_fn(move |cx| match cx.this_private(k) {
                JsValue::Number(n) => cx.ret(n),
                _ => Err(illegal()),
            }),
        )?;
        getters.push(f);
    }
    let builder = engine.evaluate_handle(
        realm,
        "(function(t,u,l){'use strict';const p={},d=(g)=>({get:g,enumerable:true,configurable:true});\
         Object.defineProperties(p,{totalJSHeapSize:d(t),usedJSHeapSize:d(u),jsHeapSizeLimit:d(l),\
         [Symbol.toStringTag]:{value:'MemoryInfo',configurable:true}});return p;})",
        lb_js::ScriptOrigin::new(""),
    )?;
    let args: Vec<Arg> = getters.iter().map(|h| Arg::Handle(*h)).collect();
    let proto = engine.call_handle(realm, builder, Arg::Undefined, &args);
    engine.release(builder);
    for h in getters {
        engine.release(h);
    }
    let proto = proto?;
    let g = engine.global(realm)?;
    let object = engine.get_property(realm, g, "Object")?;
    let create = engine.get_property(realm, object, "create")?;
    engine.release(object);
    engine.release(g);
    let stats = engine.heap_stats();
    Ok(MemoryProto {
        proto,
        create,
        mark,
        base_used: stats.used as f64,
        base_total: stats.total as f64,
    })
}

// ---------------------------------------------------------------------------
// Per-window state and registration
// ---------------------------------------------------------------------------

/// Per-realm handles this module keeps (a window is shared by its main
/// world and isolated worlds).
#[derive(Default)]
pub(crate) struct NavState {
    /// `(Promise, Promise.resolve)` of each realm, captured before page
    /// scripts run.
    pub promise_resolve: std::collections::HashMap<lb_js::RealmId, (lb_js::Handle, lb_js::Handle)>,
    /// The `getBattery()` promise of each realm.
    pub battery: std::collections::HashMap<lb_js::RealmId, lb_js::Handle>,
    /// `RTCPeerConnection`s created so far (seeds their ICE strings).
    pub peers: u32,
    /// `icecandidate` events: event object id → candidate object id.
    pub ice_events: std::collections::HashMap<u32, Option<u32>>,
    /// `performance.memory` support of each main-world realm.
    pub memory: std::collections::HashMap<lb_js::RealmId, MemoryProto>,
}

/// Capture the realm's intrinsic `Promise` and `Promise.resolve` (before
/// page scripts can replace them).
pub(crate) fn setup_realm(
    w: &Window,
    engine: &mut dyn lb_js::RealmOps,
    realm: lb_js::RealmId,
) -> Result<(), lb_js::JsError> {
    let g = engine.global(realm)?;
    let promise = engine.get_property(realm, g, "Promise")?;
    let resolve = engine.get_property(realm, promise, "resolve")?;
    engine.release(g);
    let mem = setup_memory(engine, realm)?;
    let mut st = w.nav_state.borrow_mut();
    st.promise_resolve.insert(realm, (promise, resolve));
    st.memory.insert(realm, mem);
    Ok(())
}

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    // Navigator members.
    getter(reg, rt, "Navigator", "plugins", |cx, w| {
        this_obj(cx)?;
        cx.ret(single(w, "nav:plugins", 0, || NavObj::PluginArray))
    });
    getter(reg, rt, "Navigator", "mimeTypes", |cx, w| {
        this_obj(cx)?;
        cx.ret(single(w, "nav:mimetypes", 0, || NavObj::MimeTypeArray))
    });
    getter(reg, rt, "Navigator", "userAgentData", |cx, w| {
        this_obj(cx)?;
        cx.ret(new_nav(w, NavObj::UaData))
    });
    getter(reg, rt, "Navigator", "connection", |cx, w| {
        this_obj(cx)?;
        cx.ret(single(w, "nav:connection", 0, || NavObj::Connection))
    });
    getter(reg, rt, "Navigator", "permissions", |cx, w| {
        this_obj(cx)?;
        cx.ret(single(w, "nav:permissions", 0, || NavObj::Permissions))
    });
    getter(reg, rt, "Navigator", "mediaDevices", |cx, w| {
        this_obj(cx)?;
        cx.ret(single(w, "nav:mediadevices", 0, || NavObj::MediaDevices))
    });
    method(reg, rt, "Navigator", "getGamepads", |cx, _w| {
        this_obj(cx)?;
        cx.ret(Arg::Array(vec![Arg::Null; 4]))
    });
    getter(reg, rt, "Window", "speechSynthesis", |cx, w| {
        cx.ret(single(w, "nav:speech", 0, || NavObj::Speech))
    });

    register_plugins(reg, rt);
    register_ua_data(reg, rt);
    register_connection(reg, rt);
    register_permissions(reg, rt);
    register_battery(reg, rt);
    register_media(reg, rt);
    register_speech(reg, rt);
    register_screen_extras(reg, rt);
    register_timing(reg, rt);
    super::rtc::register(reg, rt);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_names_follow_chrome() {
        assert!(matches!(
            perm_rule("camera"),
            Some(PermRule::State("video_capture", "prompt"))
        ));
        assert!(matches!(
            perm_rule("clipboard-write"),
            Some(PermRule::State("clipboard_write", "granted"))
        ));
        assert!(matches!(perm_rule("nfc"), Some(PermRule::Disabled(_))));
        assert!(perm_rule("bogus").is_none());
    }

    #[test]
    fn brands_serialize_in_order() {
        let b = vec![
            ("Google Chrome".to_owned(), "153".to_owned()),
            ("Not_A Brand".to_owned(), "8".to_owned()),
        ];
        assert_eq!(
            brands_json(&b),
            r#"[{"brand":"Google Chrome","version":"153"},{"brand":"Not_A Brand","version":"8"}]"#
        );
    }
}

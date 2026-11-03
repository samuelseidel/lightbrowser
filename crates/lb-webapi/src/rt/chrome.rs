//! Per-realm setup of a page's main world that is not WebIDL (chrome
//! build): `window.chrome` and the engine's `Intl` / `Date` defaults.
//!
//! `window.chrome` has the shape stock Chrome 153 gives a web page
//! (checked against the local Chromium 153 on loopback): an enumerable,
//! writable, configurable data property of the window, placed right after
//! `WebAssembly` in the window's own keys, holding
//! `{loadTimes, csi, app}`. `loadTimes` and `csi` are anonymous native
//! functions with a `prototype` (`function () { [native code] }`); `app`
//! has `isInstalled`, four native methods and the `InstallState` /
//! `RunningState` enums. There is no `chrome.runtime` (only extension
//! pages have it). Values come from the page clock and [`LoadInfo`].
//!
//! Time zone and locale come from the persona through V8's ICU
//! ([`lb_js::Engine::set_time_zone`]). ICU's defaults are process-wide;
//! see that method for what this means when personas with different zones
//! share a process.

use std::rc::Rc;

use lb_js::{Arg, CallCtx, HostError, JsError, RealmId, ScriptOrigin};
use lb_persona::BrowserFamily;

use super::{TaskSource, Window};
use crate::desc::GlobalProp;
use crate::registry::host_fn;

/// What the page layer knows about how the document was loaded
/// (`chrome.loadTimes()`, `chrome.csi()`). Unset fields are guessed: the
/// protocol from the URL scheme, the navigation type `Other`.
#[derive(Debug, Clone, Default)]
pub struct LoadInfo {
    /// ALPN / `connectionInfo`: `h2`, `http/1.1`, `h3`.
    pub connection_info: Option<String>,
    /// Chrome's `loadTimes().navigationType`: `Other`, `LinkClicked`,
    /// `FormSubmitted`, `BackForward`, `Reload`.
    pub navigation_type: Option<String>,
    /// Page clock ms of `DOMContentLoaded` and of the `load` event.
    pub dcl_ms: Option<f64>,
    pub load_ms: Option<f64>,
}

/// Set up a new main-world realm: ICU defaults from the persona, then
/// `window.chrome`. Runs before any page script.
pub(crate) fn install(
    w: &Rc<Window>,
    engine: &mut dyn lb_js::RealmOps,
    realm: RealmId,
) -> Result<(), JsError> {
    if let Some(p) = &w.profile {
        engine.set_default_locale(&p.locale.language);
        if !engine.set_time_zone(&p.locale.timezone) {
            w.warn(format!(
                "persona time zone {:?} is unknown to ICU; keeping {}",
                p.locale.timezone,
                lb_js::v8::icu::get_default_time_zone()
            ));
        }
    }
    if w.persona.family != BrowserFamily::Chrome {
        return Ok(());
    }
    let rt = w.rt.clone();
    let load_times = engine.new_constructible_function(
        realm,
        "",
        0,
        host_fn(move |cx| {
            let w = rt.win(cx.realm())?;
            load_times(cx, &w)
        }),
    )?;
    let rt = w.rt.clone();
    let csi = engine.new_constructible_function(
        realm,
        "",
        0,
        host_fn(move |cx| {
            let w = rt.win(cx.realm())?;
            csi(cx, &w)
        }),
    )?;
    let get_details =
        engine.new_function(realm, "getDetails", 0, host_fn(|cx| cx.ret(Arg::Null)))?;
    let get_is_installed =
        engine.new_function(realm, "getIsInstalled", 0, host_fn(|cx| cx.ret(false)))?;
    let rt = w.rt.clone();
    let install_state = engine.new_function(
        realm,
        "installState",
        0,
        host_fn(move |cx| {
            let w = rt.win(cx.realm())?;
            install_state(cx, &w)
        }),
    )?;
    let running_state =
        engine.new_function(realm, "runningState", 0, host_fn(|cx| cx.ret("cannot_run")))?;
    let later = serde_json::to_string(&later_globals()).unwrap_or_else(|_| "[]".into());
    let builder = engine.evaluate_handle(realm, BUILDER, ScriptOrigin::new(""))?;
    let r = engine.call(
        realm,
        builder,
        Arg::Undefined,
        &[
            Arg::Handle(load_times),
            Arg::Handle(csi),
            Arg::Handle(get_details),
            Arg::Handle(get_is_installed),
            Arg::Handle(install_state),
            Arg::Handle(running_state),
            Arg::Json(later),
        ],
    );
    for h in [
        builder,
        load_times,
        csi,
        get_details,
        get_is_installed,
        install_state,
        running_state,
    ] {
        engine.release(h);
    }
    r.map(|_| ())
}

/// Defines `window.chrome` and moves the window properties Chrome
/// installs after it back behind it. Runs once and leaves nothing behind.
const BUILDER: &str = "(function(lt,csi,gd,gi,is,rs,later){'use strict';\
const g=globalThis,O=Object,gpd=O.getOwnPropertyDescriptor,dp=O.defineProperty;\
const app={isInstalled:false,getDetails:gd,getIsInstalled:gi,installState:is,runningState:rs,\
InstallState:{DISABLED:'disabled',INSTALLED:'installed',NOT_INSTALLED:'not_installed'},\
RunningState:{CANNOT_RUN:'cannot_run',READY_TO_RUN:'ready_to_run',RUNNING:'running'}};\
dp(g,'chrome',{value:{loadTimes:lt,csi:csi,app:app},writable:true,enumerable:true,configurable:true});\
for(const k of later){const d=gpd(g,k);if(d&&d.configurable){delete g[k];dp(g,k,d);}}})";

/// Window property names that come after `chrome` in Chrome's order.
fn later_globals() -> Vec<&'static str> {
    let props = crate::generated::chromium::TABLE.window_props;
    let at = props
        .iter()
        .position(|p| matches!(p, GlobalProp::Other("chrome")));
    match at {
        Some(i) => props[i + 1..].iter().map(GlobalProp::name).collect(),
        None => Vec::new(),
    }
}

fn info(w: &Window) -> LoadInfo {
    w.load_info.borrow().clone()
}

fn protocol(w: &Window, li: &LoadInfo) -> String {
    li.connection_info.clone().unwrap_or_else(|| {
        if w.url().scheme() == "https" {
            "h2".into()
        } else {
            "http/1.1".into()
        }
    })
}

/// `chrome.loadTimes()`: seconds since the epoch.
fn load_times(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<(), HostError> {
    let li = info(w);
    let origin = w.clock.time_origin;
    let secs = |ms: f64| (origin + ms) / 1000.0;
    let request = secs(0.0);
    let dcl = li.dcl_ms;
    let load = li.load_ms;
    // The window is created at commit; the request started a little
    // earlier. Without the page layer's numbers, put commit a third of
    // the way to DOMContentLoaded.
    let commit = secs(dcl.map_or(0.0, |d| (d / 3.0).min(50.0)));
    let first_paint = match (load, dcl) {
        (Some(l), _) => secs(l + 12.0),
        (None, Some(d)) => secs(d + 12.0),
        _ => 0.0,
    };
    let proto = protocol(w, &li);
    let h2 = proto == "h2";
    let ms = |s: f64| (s * 1000.0).round() / 1000.0;
    let num = |v: f64| serde_json::Value::from(ms(v)).to_string();
    let nav_type = li.navigation_type.clone().unwrap_or_else(|| "Other".into());
    let npn = if h2 { proto.as_str() } else { "unknown" };
    // Keys in Chrome's order (serde_json would sort them).
    let v = format!(
        "{{\"requestTime\":{},\"startLoadTime\":{},\"commitLoadTime\":{},\
         \"finishDocumentLoadTime\":{},\"finishLoadTime\":{},\"firstPaintTime\":{},\
         \"firstPaintAfterLoadTime\":0,\"navigationType\":{},\"wasFetchedViaSpdy\":{h2},\
         \"wasNpnNegotiated\":{h2},\"npnNegotiatedProtocol\":{},\
         \"wasAlternateProtocolAvailable\":false,\"connectionInfo\":{}}}",
        num(request),
        num(request),
        num(commit),
        num(dcl.map_or(0.0, secs)),
        num(load.map_or(0.0, secs)),
        num(first_paint),
        super::js_string_literal(&nav_type),
        super::js_string_literal(npn),
        super::js_string_literal(&proto),
    );
    cx.ret(Arg::Json(v))
}

/// `chrome.csi()`: milliseconds.
fn csi(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<(), HostError> {
    let li = info(w);
    let origin = w.clock.time_origin;
    let tran = match li.navigation_type.as_deref() {
        Some("LinkClicked" | "FormSubmitted" | "FormResubmitted") => 0,
        Some("BackForward") => 6,
        Some("Reload") => 16,
        _ => 15,
    };
    let page_t = (w.clock.now() * 1000.0).round() / 1000.0;
    let onload = li.load_ms.map_or(0.0, |l| (origin + l).floor());
    let v = format!(
        "{{\"startE\":{},\"onloadT\":{},\"pageT\":{},\"tran\":{tran}}}",
        serde_json::Value::from(origin.floor()),
        serde_json::Value::from(onload),
        serde_json::Value::from(page_t),
    );
    cx.ret(Arg::Json(v))
}

/// `chrome.app.installState(callback)`: calls back later with
/// `not_installed`.
fn install_state(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<(), HostError> {
    if cx.arg_kind(0) != lb_js::ValueKind::Function {
        return Ok(());
    }
    let f = cx.arg_handle(0);
    w.queue_task(
        TaskSource::Networking,
        Box::new(move |js, w| {
            if let Err(e) = js.call(f, Arg::Undefined, &[Arg::String("not_installed".into())]) {
                w.report_exception(js, &e);
            }
            js.release(f);
        }),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chrome_comes_before_late_globals() {
        let later = later_globals();
        assert!(later.len() > 300, "{}", later.len());
        assert_eq!(later[0], "crashReport");
        assert!(!later.contains(&"chrome"));
    }
}

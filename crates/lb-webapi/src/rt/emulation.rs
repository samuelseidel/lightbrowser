//! What CDP emulation changes in a window (ADR 0039): `navigator.languages`
//! after an `acceptLanguage` override, and `navigator.geolocation`.
//!
//! Geolocation answers from [`crate::host::NetHost::geolocation`] (CDP
//! `Emulation.setGeolocationOverride`) when the persona grants the
//! `geolocation` permission. As in headless Chrome 153 (recorded in
//! `lb-page` `tests/locale/`): without the permission the error callback
//! gets `PERMISSION_DENIED` "User denied Geolocation" in a task (nobody
//! answers a prompt); `timeout: 0` gives `TIMEOUT` "Timeout expired" at
//! once; an override without a position gives `POSITION_UNAVAILABLE` with
//! an empty message; without an override nothing answers until the
//! timeout. Each new override (not a clear) first fails every running
//! request with `POSITION_UNAVAILABLE` (Chrome resets the provider: a
//! waiting `getCurrentPosition` ends there), then gives watchers the new
//! answer. Not modelled: `maximumAge` (every answer is the override's
//! position).

use std::collections::BTreeMap;
use std::rc::Rc;

use lb_js::{Arg, CallCtx, Handle, HostError, ValueKind};

use super::navigator::NavObj;
use super::{Dict, Obj, Runtime, TaskSource, TimerCallback, Window, getter, method, this_obj};
use crate::host::{GeoAnswer, GeoCoords};
use crate::registry::Registry;

/// `navigator.languages` changed (a CDP `acceptLanguage` override): the
/// next read makes a new frozen array, as Blink does.
pub fn languages_changed(w: &Window) {
    w.helpers.borrow_mut().languages = None;
}

/// A task that gives the receiving worker's isolate its context's current
/// ICU time zone and default locale (its date cache and cached default
/// locale are reset), then the workers it started (ADR 0039). The values
/// are read when the task runs, so a late task never brings back an old
/// one.
pub fn intl_task() -> super::RemoteTask {
    Box::new(|js, w| {
        let ops = js.realm_ops();
        w.net.with_intl(&mut |zone, locale| {
            ops.set_default_locale(locale);
            ops.set_time_zone(zone);
        });
        notify_workers(w);
    })
}

/// Send [`intl_task`] to every worker `w` and its frames started.
pub fn notify_workers(w: &Window) {
    let mut workers = vec![];
    for d in super::frames::descendants(w) {
        workers.extend(d.child_workers());
    }
    workers.extend(w.child_workers());
    for c in workers {
        c.inbox().send(intl_task());
    }
}

/// The window's geolocation override was set (`set`) or cleared: in a
/// task, running requests fail with `POSITION_UNAVAILABLE` and watchers
/// get the new answer (see the module docs). A clear changes nothing for
/// them.
pub fn geolocation_changed(w: &Rc<Window>, set: bool) {
    if !set {
        return;
    }
    w.queue_task(
        TaskSource::Networking,
        Box::new(|js, w| {
            let running = |w: &Window| -> Vec<u64> {
                w.nav_state
                    .borrow()
                    .geo
                    .requests
                    .iter()
                    .filter(|(_, r)| r.active)
                    .map(|(id, _)| *id)
                    .collect()
            };
            for id in running(w) {
                fail(js, w, id, 2, "");
            }
            let Some(a) = w.net.geolocation() else { return };
            for id in running(w) {
                deliver(js, w, id, &a);
            }
        }),
    );
}

/// Geolocation host objects (`Obj::Nav(NavObj::Geo(..))`).
pub(crate) enum GeoObj {
    Geolocation,
    Position { timestamp: f64, coords: u32 },
    Coords(GeoCoords),
    Error { code: u16, message: &'static str },
}

impl GeoObj {
    pub(crate) fn iface_name(&self) -> &'static str {
        match self {
            GeoObj::Geolocation => "Geolocation",
            GeoObj::Position { .. } => "GeolocationPosition",
            GeoObj::Coords(_) => "GeolocationCoordinates",
            GeoObj::Error { .. } => "GeolocationPositionError",
        }
    }
}

/// A window's waiting requests and watchers.
#[derive(Default)]
pub(crate) struct GeoState {
    next_watch: i32,
    next_request: u64,
    pub(crate) requests: BTreeMap<u64, Request>,
}

pub(crate) struct Request {
    success: Handle,
    error: Option<Handle>,
    /// `watchPosition` id; `None` for `getCurrentPosition`.
    watch: Option<i32>,
    timer: Option<u32>,
    /// Started with the permission granted: overrides reach it.
    active: bool,
}

const DENIED: &str = "User denied Geolocation";
const INSECURE: &str = "Only secure origins are allowed (see: https://goo.gl/Y0ZkNV).";
const TIMEOUT: &str = "Timeout expired";

fn geo<R>(w: &Window, id: u32, f: impl FnOnce(&GeoObj) -> Option<R>) -> Option<R> {
    w.with_obj(id, |o| match o {
        Obj::Nav(n) => match &**n {
            NavObj::Geo(g) => f(g),
            _ => None,
        },
        _ => None,
    })
    .flatten()
}

fn this_geo<R>(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    f: impl FnOnce(&GeoObj) -> Option<R>,
) -> Result<R, HostError> {
    let id = this_obj(cx)?;
    geo(w, id, f).ok_or_else(super::illegal)
}

fn new_geo(w: &Window, o: GeoObj) -> u32 {
    w.new_obj(Obj::Nav(Box::new(NavObj::Geo(Box::new(o)))))
}

fn null_or(v: Option<f64>) -> Arg {
    v.map_or(Arg::Null, Arg::from)
}

fn coords_json(c: &GeoCoords) -> String {
    let n = |v: Option<f64>| v.map_or(serde_json::Value::Null, serde_json::Value::from);
    serde_json::json!({
        "accuracy": c.accuracy,
        "latitude": c.latitude,
        "longitude": c.longitude,
        "altitude": n(c.altitude),
        "altitudeAccuracy": n(c.altitude_accuracy),
        "heading": n(c.heading),
        "speed": n(c.speed),
    })
    .to_string()
}

/// `[Clamp] unsigned long` of a `PositionOptions` member.
fn clamp_u32(v: f64) -> f64 {
    if v.is_nan() {
        0.0
    } else {
        v.clamp(0.0, f64::from(u32::MAX)).round_ties_even()
    }
}

/// `getCurrentPosition` / `watchPosition`: check the arguments, register
/// the request and start it in a task. Returns the request id.
fn request(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Rc<Window>,
    name: &str,
    watch: bool,
) -> Result<(u64, Option<i32>), HostError> {
    this_geo(cx, w, |g| matches!(g, GeoObj::Geolocation).then_some(()))?;
    let pre = format!("Failed to execute '{name}' on 'Geolocation': ");
    if cx.is_empty() {
        return Err(HostError::type_error(format!(
            "{pre}1 argument required, but only 0 present."
        )));
    }
    if cx.arg_kind(0) != ValueKind::Function {
        return Err(HostError::type_error(format!(
            "{pre}parameter 1 is not of type 'Function'."
        )));
    }
    let error = match (cx.len() > 1).then(|| cx.arg_kind(1)) {
        None | Some(ValueKind::Undefined | ValueKind::Null) => None,
        Some(ValueKind::Function) => Some(()),
        Some(_) => {
            return Err(HostError::type_error(format!(
                "{pre}parameter 2 is not of type 'Function'."
            )));
        }
    };
    let timeout = match (cx.len() > 2).then(|| cx.arg_kind(2)) {
        None | Some(ValueKind::Undefined | ValueKind::Null) => f64::from(u32::MAX),
        Some(ValueKind::Object | ValueKind::Function) => {
            let d = Dict::arg(cx, 2);
            let _ = d.number(cx, "enableHighAccuracy");
            let t = d.number(cx, "timeout");
            let _ = d.number(cx, "maximumAge");
            d.release(cx);
            t?.map_or(f64::from(u32::MAX), clamp_u32)
        }
        Some(_) => {
            return Err(HostError::type_error(format!(
                "{pre}The provided value is not of type 'PositionOptions'."
            )));
        }
    };
    let success = cx.arg_handle(0);
    let error = error.map(|()| cx.arg_handle(1));
    let (id, watch_id) = {
        let mut st = w.nav_state.borrow_mut();
        let g = &mut st.geo;
        g.next_request += 1;
        let watch_id = watch.then(|| {
            g.next_watch += 1;
            g.next_watch
        });
        g.requests.insert(
            g.next_request,
            Request {
                success,
                error,
                watch: watch_id,
                timer: None,
                active: false,
            },
        );
        (g.next_request, watch_id)
    };
    w.queue_task(
        TaskSource::Networking,
        Box::new(move |js, w| start(js, w, id, timeout)),
    );
    Ok((id, watch_id))
}

/// The first answer of request `id`.
fn start(js: &mut dyn super::Js, w: &Rc<Window>, id: u64, timeout: f64) {
    if !w.nav_state.borrow().geo.requests.contains_key(&id) {
        return;
    }
    if !super::is_secure_url(&super::frames::effective_url(w)) {
        return fail(js, w, id, 1, INSECURE);
    }
    if super::navigator::persona_state(w, "geolocation", "prompt") != "granted" {
        return fail(js, w, id, 1, DENIED);
    }
    if timeout == 0.0 {
        return fail(js, w, id, 3, TIMEOUT);
    }
    if let Some(r) = w.nav_state.borrow_mut().geo.requests.get_mut(&id) {
        r.active = true;
    }
    if let Some(a) = w.net.geolocation() {
        return deliver(js, w, id, &a);
    }
    if timeout < f64::from(u32::MAX) {
        let t = w.add_timer(
            TimerCallback::Task(Some(Box::new(move |js, w| {
                let waiting = w
                    .nav_state
                    .borrow_mut()
                    .geo
                    .requests
                    .get_mut(&id)
                    .is_some_and(|r| r.timer.take().is_some());
                if waiting {
                    fail(js, w, id, 3, TIMEOUT);
                }
            }))),
            timeout,
            Vec::new(),
            false,
        );
        if let Some(r) = w.nav_state.borrow_mut().geo.requests.get_mut(&id) {
            r.timer = Some(t);
        }
    }
}

fn deliver(js: &mut dyn super::Js, w: &Rc<Window>, id: u64, a: &GeoAnswer) {
    match a {
        GeoAnswer::Unavailable => fail(js, w, id, 2, ""),
        GeoAnswer::Position(c) => {
            let Some((cb, watch)) = settle(w, id) else {
                return;
            };
            let coords = new_geo(w, GeoObj::Coords(c.clone()));
            let pos = new_geo(
                w,
                GeoObj::Position {
                    timestamp: c.timestamp,
                    coords,
                },
            );
            call(js, w, Some(cb), w.obj_arg(pos));
            if watch.is_none() {
                drop_request(js, w, id);
            }
        }
    }
}

fn fail(js: &mut dyn super::Js, w: &Rc<Window>, id: u64, code: u16, message: &'static str) {
    let Some((_, watch)) = settle(w, id) else {
        return;
    };
    let cb = w
        .nav_state
        .borrow()
        .geo
        .requests
        .get(&id)
        .and_then(|r| r.error);
    let err = new_geo(w, GeoObj::Error { code, message });
    call(js, w, cb, w.obj_arg(err));
    if watch.is_none() {
        drop_request(js, w, id);
    }
}

/// The success callback and watch id of a waiting request; a one-shot
/// stops waiting (its timer is cancelled).
fn settle(w: &Rc<Window>, id: u64) -> Option<(Handle, Option<i32>)> {
    let mut st = w.nav_state.borrow_mut();
    let r = st.geo.requests.get_mut(&id)?;
    let timer = r.timer.take();
    let out = (r.success, r.watch);
    drop(st);
    if let Some(t) = timer {
        w.timers.borrow_mut().remove(&t);
    }
    Some(out)
}

fn call(js: &mut dyn super::Js, w: &Rc<Window>, cb: Option<Handle>, arg: Arg) {
    if let Some(f) = cb
        && let Err(e) = js.call(f, Arg::Undefined, &[arg])
    {
        w.report_exception(js, &e);
    }
}

fn drop_request(js: &mut dyn super::Js, w: &Rc<Window>, id: u64) {
    let r = w.nav_state.borrow_mut().geo.requests.remove(&id);
    if let Some(r) = r {
        js.release(r.success);
        if let Some(e) = r.error {
            js.release(e);
        }
        if let Some(t) = r.timer {
            w.timers.borrow_mut().remove(&t);
        }
    }
}

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    getter(reg, rt, "Navigator", "geolocation", |cx, w| {
        this_obj(cx)?;
        cx.ret(super::navigator::single_nav(
            w,
            "nav:geolocation",
            NavObj::Geo(Box::new(GeoObj::Geolocation)),
        ))
    });
    method(reg, rt, "Geolocation", "getCurrentPosition", |cx, w| {
        request(cx, w, "getCurrentPosition", false)?;
        cx.ret(Arg::Undefined)
    });
    method(reg, rt, "Geolocation", "watchPosition", |cx, w| {
        let (_, id) = request(cx, w, "watchPosition", true)?;
        cx.ret(id.unwrap_or(0))
    });
    method(reg, rt, "Geolocation", "clearWatch", |cx, w| {
        this_geo(cx, w, |g| matches!(g, GeoObj::Geolocation).then_some(()))?;
        if cx.is_empty() {
            return Err(HostError::type_error(
                "Failed to execute 'clearWatch' on 'Geolocation': 1 argument required, but only 0 present.",
            ));
        }
        let watch = cx.arg_i32(0)?;
        let found = w
            .nav_state
            .borrow()
            .geo
            .requests
            .iter()
            .find(|(_, r)| r.watch == Some(watch))
            .map(|(id, _)| *id);
        if let Some(id) = found {
            drop_request(cx, w, id);
        }
        cx.ret(Arg::Undefined)
    });
    getter(reg, rt, "GeolocationPosition", "timestamp", |cx, w| {
        let t = this_geo(cx, w, |g| match g {
            GeoObj::Position { timestamp, .. } => Some(*timestamp),
            _ => None,
        })?;
        cx.ret(t)
    });
    getter(reg, rt, "GeolocationPosition", "coords", |cx, w| {
        let c = this_geo(cx, w, |g| match g {
            GeoObj::Position { coords, .. } => Some(*coords),
            _ => None,
        })?;
        cx.ret(w.obj_arg(c))
    });
    method(reg, rt, "GeolocationPosition", "toJSON", |cx, w| {
        let (t, c) = this_geo(cx, w, |g| match g {
            GeoObj::Position { timestamp, coords } => Some((*timestamp, *coords)),
            _ => None,
        })?;
        let coords = geo(w, c, |g| match g {
            GeoObj::Coords(c) => Some(coords_json(c)),
            _ => None,
        })
        .unwrap_or_else(|| "{}".into());
        cx.ret(Arg::Json(format!(
            "{{\"timestamp\":{t},\"coords\":{coords}}}"
        )))
    });
    macro_rules! coord {
        ($name:literal, |$c:ident| $e:expr) => {
            getter(reg, rt, "GeolocationCoordinates", $name, |cx, w| {
                let v = this_geo(cx, w, |g| match g {
                    GeoObj::Coords($c) => Some($e),
                    _ => None,
                })?;
                cx.ret(v)
            });
        };
    }
    coord!("latitude", |c| Arg::from(c.latitude));
    coord!("longitude", |c| Arg::from(c.longitude));
    coord!("accuracy", |c| Arg::from(c.accuracy));
    coord!("altitude", |c| null_or(c.altitude));
    coord!("altitudeAccuracy", |c| null_or(c.altitude_accuracy));
    coord!("heading", |c| null_or(c.heading));
    coord!("speed", |c| null_or(c.speed));
    method(reg, rt, "GeolocationCoordinates", "toJSON", |cx, w| {
        let j = this_geo(cx, w, |g| match g {
            GeoObj::Coords(c) => Some(coords_json(c)),
            _ => None,
        })?;
        cx.ret(Arg::Json(j))
    });
    getter(reg, rt, "GeolocationPositionError", "code", |cx, w| {
        let c = this_geo(cx, w, |g| match g {
            GeoObj::Error { code, .. } => Some(*code),
            _ => None,
        })?;
        cx.ret(u32::from(c))
    });
    getter(reg, rt, "GeolocationPositionError", "message", |cx, w| {
        let m = this_geo(cx, w, |g| match g {
            GeoObj::Error { message, .. } => Some(*message),
            _ => None,
        })?;
        cx.ret(m)
    });
}

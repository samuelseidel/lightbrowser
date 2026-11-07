//! Window members: console, timers, animation frames, Location, History,
//! Navigator, Storage, Performance, Screen, window geometry and the small
//! globals (`atob`, `postMessage`, `structuredClone`, ...).

use std::rc::Rc;

use lb_dom::{Document, NodeFlags, NodeId, local_name};
use lb_js::{Arg, CallCtx, HostError, HostRef, JsError, JsValue, RealmId, ScriptOrigin, ValueKind};
use url::Url;

use super::events::{EventExtra, coarse_now, fire_event_with};
use super::{
    HistEntry, Js, K_OBJ, NavRequest, Obj, Runtime, TaskSource, TimerCallback, WINDOW_REF, Window,
    getter, illegal, method, need_args, setter, this_obj,
};
use crate::host::ConsoleMessage;
use crate::registry::{Registry, host_fn};

// ---------------------------------------------------------------------------
// console
// ---------------------------------------------------------------------------

/// Replace the message-producing methods of V8's `console` with native
/// functions that log into the window. Same names, lengths and property
/// positions as V8's own.
pub(crate) fn install_console(
    engine: &mut dyn lb_js::RealmOps,
    realm: RealmId,
    rt: &Runtime,
) -> Result<(), JsError> {
    let g = engine.global(realm)?;
    let console = engine.get_property(realm, g, "console")?;
    for level in [
        "debug", "error", "info", "log", "warn", "dir", "dirxml", "table", "trace", "assert",
    ] {
        let rt = rt.clone();
        let f = engine.new_function(
            realm,
            level,
            0,
            host_fn(move |cx| {
                let w = rt.win(cx.realm())?;
                console_call(cx, &w, level)
            }),
        )?;
        engine.set_property(realm, console, level, Arg::Handle(f))?;
        engine.release(f);
    }
    engine.release(console);
    engine.release(g);
    Ok(())
}

fn console_call(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    level: &'static str,
) -> Result<(), HostError> {
    let mut start = 0;
    let mut prefix = String::new();
    if level == "assert" {
        if cx.arg_bool(0) {
            return Ok(());
        }
        start = 1;
        prefix.push_str("Assertion failed");
    }
    let mut parts: Vec<String> = Vec::new();
    for i in start..cx.len() {
        parts.push(format_arg(cx, i));
    }
    // printf-style substitutions in a leading string.
    let text = if let Some(first) = parts
        .first()
        .filter(|_| cx.arg_kind(start) == ValueKind::String && parts[0].contains('%'))
    {
        let mut out = String::new();
        let mut rest = parts[1..].iter();
        let mut chars = first.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '%'
                && let Some(&k) = chars.peek()
            {
                match k {
                    's' | 'd' | 'i' | 'f' | 'o' | 'O' | 'j' => {
                        chars.next();
                        if let Some(a) = rest.next() {
                            match k {
                                'd' | 'i' => {
                                    out.push_str(&a.parse::<f64>().map_or("NaN".to_owned(), |f| {
                                        (f.trunc() as i64).to_string()
                                    }))
                                }
                                _ => out.push_str(a),
                            }
                        }
                        continue;
                    }
                    'c' => {
                        chars.next();
                        rest.next();
                        continue;
                    }
                    '%' => {
                        chars.next();
                        out.push('%');
                        continue;
                    }
                    _ => {}
                }
            }
            out.push(c);
        }
        let tail: Vec<&str> = rest.map(String::as_str).collect();
        if !tail.is_empty() {
            out.push(' ');
            out.push_str(&tail.join(" "));
        }
        out
    } else {
        parts.join(" ")
    };
    let text = match (prefix.is_empty(), text.is_empty()) {
        (true, _) => text,
        (false, true) => prefix,
        (false, false) => format!("{prefix}: {text}"),
    };
    let level = match level {
        "warn" => "warning",
        "assert" => "error",
        "dir" | "dirxml" | "table" | "trace" => "log",
        l => l,
    };
    w.log(ConsoleMessage {
        level: level.into(),
        text,
        url: None,
        line: None,
        from_worker: false,
    });
    Ok(())
}

fn format_arg(cx: &mut CallCtx<'_, '_, '_>, i: usize) -> String {
    match cx.arg_kind(i) {
        ValueKind::String => cx.arg_string(i).unwrap_or_default(),
        ValueKind::Undefined => "undefined".into(),
        ValueKind::Null => "null".into(),
        ValueKind::Object => match cx.arg_value(i) {
            JsValue::Json(s) if s != "{}" => s,
            _ => cx.arg_string(i).unwrap_or_default(),
        },
        _ => cx.arg_string(i).unwrap_or_default(),
    }
}

// ---------------------------------------------------------------------------
// Timers
// ---------------------------------------------------------------------------

/// Run the earliest due timer.
pub(crate) fn run_due_timer(js: &mut dyn Js, w: &Rc<Window>) -> bool {
    let now = w.clock.now();
    let id = {
        let timers = w.timers.borrow();
        timers
            .iter()
            .filter(|(_, t)| t.due <= now)
            .min_by(|a, b| a.1.due.total_cmp(&b.1.due).then(a.1.seq.cmp(&b.1.seq)))
            .map(|(id, _)| *id)
    };
    let Some(id) = id else { return false };
    let Some(t) = w.timers.borrow_mut().remove(&id) else {
        return false;
    };
    let saved = w.timer_nesting.replace(t.nesting);
    let this = w.host_arg(WINDOW_REF);
    let mut t = t;
    let result = match &mut t.callback {
        TimerCallback::Task(task) => {
            if let Some(task) = task.take() {
                task(js, w);
            }
            Ok(())
        }
        TimerCallback::Function(f) => {
            let args: Vec<Arg> = t.args.iter().map(|h| Arg::Handle(*h)).collect();
            js.call(*f, this, &args).map(|_| ())
        }
        TimerCallback::Code(src) => {
            let url = w.url().to_string();
            js.eval(src, ScriptOrigin::new(&url)).map(|_| ())
        }
    };
    w.timer_nesting.set(saved);
    if let Err(e) = result {
        w.report_exception(js, &e);
    }
    match t.interval {
        // Re-arm unless cleared from inside the callback.
        Some(_) if !w.is_closed() && w.cleared_during_run(id) => release_timer(js, t),
        Some(iv) if !w.is_closed() => {
            t.due = w.clock.now() + iv.max(if t.nesting >= 5 { 4.0 } else { 0.0 });
            t.nesting += 1;
            w.timers.borrow_mut().insert(id, t);
        }
        _ => release_timer(js, t),
    }
    true
}

fn release_timer(js: &mut dyn Js, t: super::Timer) {
    if let TimerCallback::Function(f) = t.callback {
        js.release(f);
    }
    for h in t.args {
        js.release(h);
    }
}

impl Window {
    /// Whether `clearInterval(id)` ran while the interval's callback ran.
    fn cleared_during_run(&self, id: u32) -> bool {
        self.cleared.borrow_mut().remove(&id)
    }
}

fn set_timer(cx: &mut CallCtx<'_, '_, '_>, w: &Window, repeat: bool) -> Result<(), HostError> {
    // A removed frame's window schedules nothing (Chrome returns 0).
    if super::frames::is_detached(w) {
        return cx.ret(0);
    }
    let cb = if cx.arg_kind(0) == ValueKind::Function {
        TimerCallback::Function(cx.arg_handle(0))
    } else {
        TimerCallback::Code(cx.arg_string(0)?)
    };
    let delay = if cx.len() > 1 { cx.arg_f64(1)? } else { 0.0 };
    let args = (2..cx.len()).map(|i| cx.arg_handle(i)).collect();
    let id = w.add_timer(cb, delay, args, repeat);
    cx.ret(id)
}

fn clear_timer(cx: &mut CallCtx<'_, '_, '_>, w: &Rc<Window>) -> Result<(), HostError> {
    let id = cx.arg_i32(0)? as u32;
    let t = w.timers.borrow_mut().remove(&id);
    match t {
        Some(t) => {
            if let TimerCallback::Function(f) = t.callback {
                cx.release(f);
            }
            for h in t.args {
                cx.release(h);
            }
        }
        // Possibly the running interval: do not re-arm it.
        None => {
            w.cleared.borrow_mut().insert(id);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers used by other modules
// ---------------------------------------------------------------------------

/// `document.lastModified` format (`MM/DD/YYYY hh:mm:ss`, local = UTC).
pub(crate) fn last_modified_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (y, m, d) = civil_from_days((secs / 86400) as i64);
    let s = secs % 86400;
    format!(
        "{m:02}/{d:02}/{y:04} {:02}:{:02}:{:02}",
        s / 3600,
        (s / 60) % 60,
        s % 60
    )
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub(crate) use crate::text::inner_text;

/// `element.click()`: a synthetic `click` plus default actions for
/// checkboxes, radios, links, labels and submit buttons.
pub(crate) fn synthetic_click(js: &mut dyn Js, w: &Rc<Window>, slot: u32, n: NodeId) {
    super::input::synthetic_click(js, w, slot, n);
}

/// `:focus-within` on the focused element's shadow-including inclusive
/// ancestors, and `:focus` on the shadow hosts it is inside (Chrome
/// matches `:focus` on a host whose shadow tree holds the focus).
fn focus_chain(d: &mut lb_dom::Document, n: NodeId, on: bool) {
    let mut cur = Some(n);
    while let Some(x) = cur {
        if d.is_element(x) {
            d.set_flags(x, NodeFlags::FOCUS_WITHIN, on);
        }
        if let Some(h) = d.shadow_host(x) {
            d.set_flags(h, NodeFlags::FOCUS, on);
        }
        cur = d.parent_or_host(x);
    }
}

/// Move focus (`focus()` / `blur()`), firing blur/focusout and
/// focus/focusin.
pub(crate) fn focus(js: &mut dyn Js, w: &Rc<Window>, to: Option<NodeId>) {
    let old = w.focused.get();
    if old == to {
        return;
    }
    let focus_extra = |related: Option<NodeId>| EventExtra::Focus {
        related: related.map(|r| w.node_ref(0, r)),
    };
    if let Some(o) = old {
        super::input::before_blur(js, w, o);
        w.with_doc_mut(|d| {
            d.set_flags(o, NodeFlags::FOCUS | NodeFlags::FOCUS_VISIBLE, false);
            focus_chain(d, o, false);
        });
        w.focused.set(None);
        fire_event_with(
            js,
            w,
            w.node_ref(0, o),
            "FocusEvent",
            "blur",
            false,
            false,
            focus_extra(to),
        );
        fire_event_with(
            js,
            w,
            w.node_ref(0, o),
            "FocusEvent",
            "focusout",
            true,
            false,
            focus_extra(to),
        );
    }
    if let Some(n) = to {
        w.with_doc_mut(|d| {
            d.set_flags(n, NodeFlags::FOCUS, true);
            focus_chain(d, n, true);
        });
        w.focused.set(Some(n));
        super::input::after_focus(w, n);
        fire_event_with(
            js,
            w,
            w.node_ref(0, n),
            "FocusEvent",
            "focus",
            false,
            false,
            focus_extra(old),
        );
        fire_event_with(
            js,
            w,
            w.node_ref(0, n),
            "FocusEvent",
            "focusin",
            true,
            false,
            focus_extra(old),
        );
    }
}

/// The form data set of `form` (submittable controls in tree order, with
/// form-associated custom elements' values), as name/value pairs.
pub(crate) fn form_entries(
    w: &Window,
    d: &lb_dom::Document,
    form: NodeId,
) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for c in d.descendants(form) {
        if d.flags(c).contains(NodeFlags::FORM_ASSOCIATED) {
            pairs.extend(super::internals::submission_entries(w, d, c));
            continue;
        }
        let Some(name) = d.get_attribute(c, "name").filter(|n| !n.is_empty()) else {
            continue;
        };
        if d.has_attribute(c, "disabled") {
            continue;
        }
        let local = d.local_name(c).map(|l| l.to_string()).unwrap_or_default();
        let value = match local.as_str() {
            "input" => {
                let t = super::dom::input_type(d, c);
                if matches!(t.as_str(), "checkbox" | "radio")
                    && !super::dom::checked(d, c, "checked")
                {
                    continue;
                }
                if matches!(t.as_str(), "submit" | "button" | "reset" | "image" | "file") {
                    continue;
                }
                w.values.borrow().get(&(0, c)).cloned().unwrap_or_else(|| {
                    d.get_attribute(c, "value")
                        .map(str::to_owned)
                        .unwrap_or_else(|| {
                            if matches!(t.as_str(), "checkbox" | "radio") {
                                "on".into()
                            } else {
                                String::new()
                            }
                        })
                })
            }
            "textarea" => w
                .values
                .borrow()
                .get(&(0, c))
                .cloned()
                .unwrap_or_else(|| d.text_content(c).unwrap_or_default()),
            "select" => {
                let opts: Vec<NodeId> = d
                    .descendants(c)
                    .filter(|&o| d.is_html_element_named(o, &local_name!("option")))
                    .collect();
                let sel = opts
                    .iter()
                    .copied()
                    .find(|&o| super::dom::checked(d, o, "selected"))
                    .or(opts.first().copied());
                match sel {
                    Some(o) => d
                        .get_attribute(o, "value")
                        .map(str::to_owned)
                        .unwrap_or_else(|| d.text_content(o).unwrap_or_default().trim().to_owned()),
                    None => continue,
                }
            }
            _ => continue,
        };
        pairs.push((name.to_owned(), value));
    }
    pairs
}

/// Navigate this window: fragment changes stay in the document, anything
/// else goes to the page layer.
pub(crate) fn navigate_to(js: &mut dyn Js, w: &Rc<Window>, url: Url, replace: bool) {
    let cur = w.url();
    let same_doc = url.fragment().is_some() && {
        let (mut a, mut b) = (cur.clone(), url.clone());
        a.set_fragment(None);
        b.set_fragment(None);
        a == b
    };
    if same_doc {
        let old = cur.to_string();
        let new = url.to_string();
        {
            let mut h = w.history.borrow_mut();
            let idx = h.index;
            if replace {
                h.entries[idx] = HistEntry {
                    url: url.clone(),
                    state: None,
                };
            } else {
                h.entries.truncate(idx + 1);
                h.entries.push(HistEntry {
                    url: url.clone(),
                    state: None,
                });
                h.index += 1;
            }
        }
        w.set_url(url);
        if old != new {
            w.queue_task(
                TaskSource::DomManipulation,
                Box::new(move |js, w| {
                    fire_event_with(
                        js,
                        w,
                        WINDOW_REF,
                        "HashChangeEvent",
                        "hashchange",
                        false,
                        false,
                        EventExtra::HashChange { old, new },
                    );
                }),
            );
        }
        let _ = js;
        return;
    }
    w.request_navigation(NavRequest::get(url, replace));
}

/// `(screenX, screenY)`: the persona's `window_position()` (Wayland 0,0;
/// else `screen.window_x/y` or the work-area origin).
pub(crate) fn window_position(w: &Window) -> (i32, i32) {
    w.profile
        .as_deref()
        .map_or((0, 0), lb_persona::PersonaProfile::window_position)
}

pub(crate) fn screen_dims(w: &Window) -> lb_persona::profile::ScreenInfo {
    w.profile
        .as_ref()
        .map(|p| p.screen.clone())
        .unwrap_or(lb_persona::profile::ScreenInfo {
            width: 1920,
            height: 1080,
            avail_width: 1920,
            avail_height: 1040,
            color_depth: 24,
            pixel_depth: 24,
            device_pixel_ratio: 1.0,
            inner_width: 1920,
            inner_height: 947,
            outer_width: 1920,
            outer_height: 1040,
            avail_top: 0,
            avail_left: 0,
            window_x: None,
            window_y: None,
            maximized: None,
        })
}

// ---------------------------------------------------------------------------
// Base64
// ---------------------------------------------------------------------------

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// `btoa` of the window or a worker scope (`iface` in messages).
pub(crate) fn btoa_impl(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    iface: &str,
) -> Result<(), HostError> {
    need_args(cx, 1, "btoa", iface)?;
    let s = cx.arg_string(0)?;
    let mut bytes = Vec::with_capacity(s.len());
    for c in s.chars() {
        if c as u32 > 0xFF {
            return Err(w.rt.err(
                "InvalidCharacterError",
                format!("Failed to execute 'btoa' on '{iface}': The string to be encoded contains characters outside of the Latin1 range."),
            ));
        }
        bytes.push(c as u32 as u8);
    }
    cx.ret(base64_encode(&bytes))
}

/// `atob` of the window or a worker scope.
pub(crate) fn atob_impl(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    iface: &str,
) -> Result<(), HostError> {
    need_args(cx, 1, "atob", iface)?;
    let s = cx.arg_string(0)?;
    match base64_decode(&s) {
        Some(b) => cx.ret(b.into_iter().map(char::from).collect::<String>()),
        None => Err(w.rt.err(
            "InvalidCharacterError",
            format!("Failed to execute 'atob' on '{iface}': The string to be decoded is not correctly encoded."),
        )),
    }
}

pub(crate) fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(B64[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Forgiving base64 decode (HTML spec); `None` on invalid input.
pub(crate) fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut s: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if s.len().is_multiple_of(4) {
        if s.ends_with(b"==") {
            s.truncate(s.len() - 2);
        } else if s.ends_with(b"=") {
            s.truncate(s.len() - 1);
        }
    }
    if s.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0;
    for b in s {
        let v = B64.iter().position(|&c| c == b)? as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

fn singleton(w: &Window, name: &'static str, make: impl FnOnce() -> Obj) -> Arg {
    let id = w.cached_obj((0, 0, name, String::new()), make);
    w.obj_arg(id)
}

fn storage_of(w: &Window, id: u32) -> Option<crate::host::StorageArea> {
    let local = w
        .with_obj(id, |o| match o {
            Obj::Storage { local } => Some(*local),
            _ => None,
        })
        .flatten()?;
    Some(if local {
        w.local_storage.clone()
    } else {
        w.session_storage.clone()
    })
}

/// History of a removed frame's document: Chrome throws.
fn not_fully_active(w: &Window, prop: &str) -> Result<(), HostError> {
    if super::frames::is_detached(w) {
        return Err(w.rt.err(
            "SecurityError",
            format!(
                "Failed to read the '{prop}' property from 'History': May not use a History object associated with a Document that is not fully active"
            ),
        ));
    }
    Ok(())
}

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    // Timers and callbacks
    method(reg, rt, "Window", "setTimeout", |cx, w| {
        set_timer(cx, w, false)
    });
    method(reg, rt, "Window", "setInterval", |cx, w| {
        set_timer(cx, w, true)
    });
    method(reg, rt, "Window", "clearTimeout", clear_timer);
    method(reg, rt, "Window", "clearInterval", clear_timer);
    method(reg, rt, "Window", "queueMicrotask", |cx, _w| {
        need_args(cx, 1, "queueMicrotask", "Window")?;
        if cx.arg_kind(0) != ValueKind::Function {
            return Err(HostError::type_error(
                "Failed to execute 'queueMicrotask' on 'Window': The callback provided as parameter 1 is not a function.",
            ));
        }
        let f = cx.arg_handle(0);
        cx.enqueue_microtask(f)
    });
    method(reg, rt, "Window", "requestAnimationFrame", |cx, w| {
        need_args(cx, 1, "requestAnimationFrame", "Window")?;
        if cx.arg_kind(0) != ValueKind::Function {
            return Err(HostError::type_error(
                "Failed to execute 'requestAnimationFrame' on 'Window': The callback provided as parameter 1 is not a function.",
            ));
        }
        if super::frames::is_detached(w) {
            return cx.ret(0);
        }
        let f = cx.arg_handle(0);
        cx.ret(w.add_raf(f))
    });
    method(reg, rt, "Window", "cancelAnimationFrame", |cx, w| {
        let id = cx.arg_u32(0)?;
        let removed: Vec<_> = {
            let mut raf = w.raf.borrow_mut();
            let (gone, keep): (Vec<_>, Vec<_>) = raf.drain(..).partition(|(i, _)| *i == id);
            *raf = keep;
            gone
        };
        for (_, h) in removed {
            cx.release(h);
        }
        Ok(())
    });
    method(reg, rt, "Window", "requestIdleCallback", |cx, w| {
        need_args(cx, 1, "requestIdleCallback", "Window")?;
        if super::frames::is_detached(w) {
            return cx.ret(0);
        }
        let f = cx.arg_handle(0);
        let id = w.add_timer(TimerCallback::Function(f), 1.0, Vec::new(), false);
        cx.ret(id)
    });
    method(reg, rt, "Window", "cancelIdleCallback", clear_timer);

    // Window geometry
    macro_rules! win_num {
        ($name:literal, |$w:ident| $e:expr) => {
            getter(reg, rt, "Window", $name, |cx, $w| {
                // A removed frame has no geometry (Chrome: 0).
                if super::frames::is_detached($w) {
                    return cx.ret(0);
                }
                let v: f64 = $e;
                cx.ret(v)
            });
        };
    }
    win_num!("innerWidth", |w| w.with_layout(0, |l, _| l.viewport().0)
        as f64);
    win_num!("innerHeight", |w| w.with_layout(0, |l, _| l.viewport().1)
        as f64);
    win_num!("outerWidth", |w| f64::from(screen_dims(w).outer_width));
    win_num!("outerHeight", |w| f64::from(screen_dims(w).outer_height));
    win_num!("devicePixelRatio", |w| screen_dims(w).device_pixel_ratio);
    win_num!("screenX", |w| f64::from(window_position(w).0));
    win_num!("screenLeft", |w| f64::from(window_position(w).0));
    win_num!("screenY", |w| f64::from(window_position(w).1));
    win_num!("screenTop", |w| f64::from(window_position(w).1));
    win_num!("scrollX", |w| w.with_layout(0, |l, d| l.window_scroll(d).0)
        as f64);
    win_num!(
        "pageXOffset",
        |w| w.with_layout(0, |l, d| l.window_scroll(d).0) as f64
    );
    win_num!("scrollY", |w| w.with_layout(0, |l, d| l.window_scroll(d).1)
        as f64);
    win_num!(
        "pageYOffset",
        |w| w.with_layout(0, |l, d| l.window_scroll(d).1) as f64
    );
    for (name, relative) in [("scrollTo", false), ("scroll", false), ("scrollBy", true)] {
        method(reg, rt, "Window", name, move |cx, w| {
            let (mut x, mut y) = (None, None);
            if cx.arg_kind(0) == ValueKind::Object {
                let d = super::Dict::arg(cx, 0);
                x = d.number(cx, "left")?;
                y = d.number(cx, "top")?;
                d.release(cx);
            } else if !cx.is_empty() {
                x = Some(cx.arg_f64(0)?);
                y = Some(if cx.len() > 1 { cx.arg_f64(1)? } else { 0.0 });
            }
            w.with_layout(0, |l, d| {
                let (cx0, cy0) = l.window_scroll(d);
                let (bx, by) = if relative { (cx0, cy0) } else { (0.0, 0.0) };
                let nx = x.map_or(cx0, |v| bx + if v.is_finite() { v as f32 } else { 0.0 });
                let ny = y.map_or(cy0, |v| by + if v.is_finite() { v as f32 } else { 0.0 });
                l.scroll_window_to(d, nx, ny)
            });
            Ok(())
        });
    }

    // Simple window members
    getter(reg, rt, "Window", "document", |cx, w| {
        cx.ret(w.node_arg(0, Document::DOCUMENT))
    });
    getter(reg, rt, "Window", "location", |cx, w| {
        cx.ret(singleton(w, "location", || Obj::Location))
    });
    setter(reg, rt, "Window", "location", |cx, w| {
        let v = cx.arg_string(0)?;
        if let Some(u) = w.resolve_url(&v) {
            navigate_to(cx, w, u, false);
        }
        Ok(())
    });
    getter(reg, rt, "Window", "history", |cx, w| {
        cx.ret(singleton(w, "history", || Obj::History))
    });
    getter(reg, rt, "Window", "navigator", |cx, w| {
        cx.ret(singleton(w, "navigator", || Obj::Navigator))
    });
    getter(reg, rt, "Window", "clientInformation", |cx, w| {
        cx.ret(singleton(w, "navigator", || Obj::Navigator))
    });
    getter(reg, rt, "Window", "localStorage", |cx, w| {
        if super::frames::is_detached(w) {
            return cx.ret(Arg::Null);
        }
        cx.ret(singleton(w, "localStorage", || Obj::Storage {
            local: true,
        }))
    });
    getter(reg, rt, "Window", "sessionStorage", |cx, w| {
        if super::frames::is_detached(w) {
            return cx.ret(Arg::Null);
        }
        cx.ret(singleton(w, "sessionStorage", || Obj::Storage {
            local: false,
        }))
    });
    getter(reg, rt, "Window", "performance", |cx, w| {
        cx.ret(singleton(w, "performance", || Obj::Performance))
    });
    getter(reg, rt, "Window", "screen", |cx, w| {
        cx.ret(singleton(w, "screen", || Obj::Screen))
    });
    getter(reg, rt, "Window", "origin", |cx, w| {
        cx.ret(w.url().origin().ascii_serialization())
    });
    getter(reg, rt, "Window", "isSecureContext", |cx, w| {
        cx.ret(super::is_secure_url(&w.url()))
    });
    getter(reg, rt, "Window", "crossOriginIsolated", |cx, _w| {
        cx.ret(false)
    });
    getter(reg, rt, "Window", "closed", |cx, w| {
        cx.ret(super::frames::is_detached(w))
    });
    getter(reg, rt, "Window", "length", |cx, _w| cx.ret(0));
    getter(reg, rt, "Window", "opener", |cx, _w| cx.ret(Arg::Null));
    getter(reg, rt, "Window", "frameElement", |cx, _w| {
        cx.ret(Arg::Null)
    });
    getter(reg, rt, "Window", "name", |cx, w| cx.ret(w.helpers_name()));
    setter(reg, rt, "Window", "name", |cx, w| {
        let v = cx.arg_string(0)?;
        w.set_helpers_name(v);
        Ok(())
    });
    getter(reg, rt, "Window", "status", |cx, _w| cx.ret(""));
    for name in [
        "alert",
        "print",
        "stop",
        "focus",
        "blur",
        "close",
        "captureEvents",
        "releaseEvents",
    ] {
        method(reg, rt, "Window", name, |_cx, _w| Ok(()));
    }
    method(reg, rt, "Window", "confirm", |cx, _w| cx.ret(false));
    method(reg, rt, "Window", "prompt", |cx, _w| cx.ret(Arg::Null));
    method(reg, rt, "Window", "open", |cx, _w| cx.ret(Arg::Null));
    method(reg, rt, "Window", "getSelection", |cx, _w| {
        cx.ret(Arg::Null)
    });
    method(reg, rt, "Window", "btoa", |cx, w| {
        btoa_impl(cx, w, "Window")
    });
    method(reg, rt, "Window", "atob", |cx, w| {
        atob_impl(cx, w, "Window")
    });
    method(reg, rt, "Window", "postMessage", |cx, w| {
        need_args(cx, 1, "postMessage", "Window")?;
        let data = cx.arg_handle(0);
        let origin = w.url().origin().ascii_serialization();
        w.queue_task(
            TaskSource::PostedMessage,
            Box::new(move |js, w| {
                fire_event_with(
                    js,
                    w,
                    WINDOW_REF,
                    "MessageEvent",
                    "message",
                    false,
                    false,
                    EventExtra::Message {
                        data: Some(data),
                        origin,
                        source: None,
                        ports: None,
                    },
                );
            }),
        );
        Ok(())
    });
    method(reg, rt, "Window", "reportError", |cx, w| {
        need_args(cx, 1, "reportError", "Window")?;
        let msg = cx.arg_string(0)?;
        let e = JsError::new(lb_js::JsErrorKind::Exception, msg.clone());
        let mut e = e;
        e.stack = Some(msg);
        super::events::report_exception(cx, w, &e);
        Ok(())
    });

    // Location
    for prop in [
        "href", "protocol", "host", "hostname", "port", "pathname", "search", "hash", "origin",
    ] {
        getter(reg, rt, "Location", prop, move |cx, w| {
            this_obj(cx)?;
            let u = w.url();
            cx.ret(if prop == "href" {
                u.to_string()
            } else {
                super::misc::url_component(&u, prop)
            })
        });
        if prop != "origin" {
            setter(reg, rt, "Location", prop, move |cx, w| {
                this_obj(cx)?;
                let v = cx.arg_string(0)?;
                let target = if prop == "href" {
                    w.resolve_url(&v)
                } else {
                    let mut u = w.url();
                    super::misc::set_url_component(&mut u, prop, &v);
                    Some(u)
                };
                match target {
                    Some(u) => {
                        navigate_to(cx, w, u, false);
                        Ok(())
                    }
                    None => Err(w.rt.err("SyntaxError", format!("Failed to set the 'href' property on 'Location': '{v}' is not a valid URL."))),
                }
            });
        }
    }
    for (name, replace) in [("assign", false), ("replace", true)] {
        method(reg, rt, "Location", name, move |cx, w| {
            need_args(cx, 1, name, "Location")?;
            let v = cx.arg_string(0)?;
            match w.resolve_url(&v) {
                Some(u) => {
                    navigate_to(cx, w, u, replace);
                    Ok(())
                }
                None => Err(w.rt.err(
                    "SyntaxError",
                    format!("Failed to execute '{name}' on 'Location': '{v}' is not a valid URL."),
                )),
            }
        });
    }
    method(reg, rt, "Location", "reload", |_cx, w| {
        w.request_navigation(NavRequest::get(w.url(), true));
        Ok(())
    });
    method(reg, rt, "Location", "toString", |cx, w| {
        cx.ret(w.url().to_string())
    });
    method(reg, rt, "Location", "valueOf", |cx, _w| {
        let this = cx.this_handle();
        cx.ret(Arg::Handle(this))
    });

    // History
    getter(reg, rt, "History", "length", |cx, w| {
        not_fully_active(w, "length")?;
        let h = w.history.borrow();
        cx.ret(h.before + h.entries.len() as u32)
    });
    getter(reg, rt, "History", "state", |cx, w| {
        not_fully_active(w, "state")?;
        let s = {
            let h = w.history.borrow();
            h.entries[h.index].state.clone()
        };
        cx.ret(s.map_or(Arg::Null, Arg::Json))
    });
    getter(reg, rt, "History", "scrollRestoration", |cx, w| {
        not_fully_active(w, "scrollRestoration")?;
        cx.ret(w.history.borrow().scroll_restoration.clone())
    });
    setter(reg, rt, "History", "scrollRestoration", |cx, w| {
        let v = cx.arg_string(0)?;
        if v == "auto" || v == "manual" {
            w.history.borrow_mut().scroll_restoration = v;
        }
        Ok(())
    });
    for (name, replace) in [("pushState", false), ("replaceState", true)] {
        method(reg, rt, "History", name, move |cx, w| {
            need_args(cx, 2, name, "History")?;
            let state = match cx.arg_kind(0) {
                ValueKind::Undefined | ValueKind::Null => None,
                _ => match cx.arg_value(0) {
                    JsValue::Json(s) => Some(s),
                    JsValue::String(s) => Some(super::js_string_literal(&s)),
                    JsValue::Number(n) => Some(if n.is_finite() { n.to_string() } else { "null".into() }),
                    JsValue::Bool(b) => Some(b.to_string()),
                    _ => return Err(w.rt.err("DataCloneError", format!("Failed to execute '{name}' on 'History': The object could not be cloned."))),
                },
            };
            let url = match super::opt_string(cx, 2)? {
                Some(s) if cx.arg_kind(2) != ValueKind::Null => {
                    let u = w.resolve_url(&s).ok_or_else(|| w.rt.err("SecurityError", format!("Failed to execute '{name}' on 'History': A history state object with URL '{s}' cannot be created in a document with origin '{}'.", w.url().origin().ascii_serialization())))?;
                    if u.origin() != w.url().origin() {
                        return Err(w.rt.err("SecurityError", format!("Failed to execute '{name}' on 'History': A history state object with URL '{u}' cannot be created in a document with origin '{}'.", w.url().origin().ascii_serialization())));
                    }
                    u
                }
                _ => w.url(),
            };
            {
                let mut h = w.history.borrow_mut();
                let entry = HistEntry {
                    url: url.clone(),
                    state,
                };
                let idx = h.index;
                if replace {
                    h.entries[idx] = entry;
                } else {
                    h.entries.truncate(idx + 1);
                    h.entries.push(entry);
                    h.index += 1;
                }
            }
            w.set_url(url);
            Ok(())
        });
    }
    for (name, delta) in [("back", -1i32), ("forward", 1), ("go", 0)] {
        method(reg, rt, "History", name, move |cx, w| {
            let delta = if name == "go" {
                if cx.is_empty() { 0 } else { cx.arg_i32(0)? }
            } else {
                delta
            };
            if delta == 0 {
                if name == "go" {
                    w.request_navigation(NavRequest::get(w.url(), true));
                }
                return Ok(());
            }
            let target = {
                let h = w.history.borrow();
                let t = h.index as i64 + i64::from(delta);
                (t >= 0 && (t as usize) < h.entries.len()).then_some(t as usize)
            };
            let Some(t) = target else { return Ok(()) };
            w.queue_task(
                TaskSource::DomManipulation,
                Box::new(move |js, w| {
                    let (old, url, state) = {
                        let mut h = w.history.borrow_mut();
                        let old = h.entries[h.index].url.clone();
                        h.index = t;
                        (old, h.entries[t].url.clone(), h.entries[t].state.clone())
                    };
                    w.set_url(url.clone());
                    fire_event_with(
                        js,
                        w,
                        WINDOW_REF,
                        "PopStateEvent",
                        "popstate",
                        false,
                        false,
                        EventExtra::PopState(state),
                    );
                    let (mut a, mut b) = (old.clone(), url.clone());
                    a.set_fragment(None);
                    b.set_fragment(None);
                    if a == b && old.fragment() != url.fragment() {
                        fire_event_with(
                            js,
                            w,
                            WINDOW_REF,
                            "HashChangeEvent",
                            "hashchange",
                            false,
                            false,
                            EventExtra::HashChange {
                                old: old.to_string(),
                                new: url.to_string(),
                            },
                        );
                    }
                }),
            );
            Ok(())
        });
    }

    // Navigator
    register_navigator(reg, rt);

    // Storage
    getter(reg, rt, "Storage", "length", |cx, w| {
        let id = this_obj(cx)?;
        cx.ret(storage_of(w, id).ok_or_else(illegal)?.len() as u32)
    });
    method(reg, rt, "Storage", "key", |cx, w| {
        need_args(cx, 1, "key", "Storage")?;
        let id = this_obj(cx)?;
        let i = cx.arg_u32(0)? as usize;
        match storage_of(w, id).ok_or_else(illegal)?.key(i) {
            Some(k) => cx.ret(k),
            None => cx.ret(Arg::Null),
        }
    });
    method(reg, rt, "Storage", "getItem", |cx, w| {
        need_args(cx, 1, "getItem", "Storage")?;
        let id = this_obj(cx)?;
        let k = cx.arg_string(0)?;
        match storage_of(w, id).ok_or_else(illegal)?.get(&k) {
            Some(v) => cx.ret(v),
            None => cx.ret(Arg::Null),
        }
    });
    method(reg, rt, "Storage", "setItem", |cx, w| {
        need_args(cx, 2, "setItem", "Storage")?;
        let id = this_obj(cx)?;
        let (k, v) = (cx.arg_string(0)?, cx.arg_string(1)?);
        storage_of(w, id).ok_or_else(illegal)?.set(&k, &v);
        Ok(())
    });
    method(reg, rt, "Storage", "removeItem", |cx, w| {
        need_args(cx, 1, "removeItem", "Storage")?;
        let id = this_obj(cx)?;
        let k = cx.arg_string(0)?;
        storage_of(w, id).ok_or_else(illegal)?.remove(&k);
        Ok(())
    });
    method(reg, rt, "Storage", "clear", |cx, w| {
        let id = this_obj(cx)?;
        storage_of(w, id).ok_or_else(illegal)?.clear();
        Ok(())
    });
    let rs = rt.clone();
    reg.named_getter(
        "Storage",
        Rc::new(move |cx, name| {
            let Some(w) = rs.window(cx.holder_realm()) else {
                return Ok(None);
            };
            let Some(h) = cx.holder_host().filter(|h| h.kind == K_OBJ) else {
                return Ok(None);
            };
            if matches!(
                name,
                "length"
                    | "key"
                    | "getItem"
                    | "setItem"
                    | "removeItem"
                    | "clear"
                    | "constructor"
                    | "toString"
                    | "valueOf"
                    | "hasOwnProperty"
            ) {
                return Ok(None);
            }
            Ok(storage_of(&w, h.id)
                .and_then(|s| s.get(name))
                .map(Arg::String))
        }),
    );
    let rs = rt.clone();
    reg.named_setter(
        "Storage",
        Rc::new(move |cx, name, value| {
            let Some(w) = rs.window(cx.holder_realm()) else {
                return Ok(false);
            };
            let Some(h) = cx.holder_host().filter(|h| h.kind == K_OBJ) else {
                return Ok(false);
            };
            let Some(s) = storage_of(&w, h.id) else {
                return Ok(false);
            };
            s.set(name, value);
            Ok(true)
        }),
    );
    let rs = rt.clone();
    reg.named_deleter(
        "Storage",
        Rc::new(move |cx, name| {
            let Some(w) = rs.window(cx.holder_realm()) else {
                return Ok(None);
            };
            let Some(h) = cx.holder_host().filter(|h| h.kind == K_OBJ) else {
                return Ok(None);
            };
            let Some(s) = storage_of(&w, h.id) else {
                return Ok(None);
            };
            Ok(s.remove(name).then_some(true))
        }),
    );
    let rs = rt.clone();
    reg.named_enumerator(
        "Storage",
        Rc::new(move |cx| {
            let Some(w) = rs.window(cx.holder_realm()) else {
                return Vec::new();
            };
            let Some(h) = cx.holder_host().filter(|h| h.kind == K_OBJ) else {
                return Vec::new();
            };
            storage_of(&w, h.id).map(|s| s.keys()).unwrap_or_default()
        }),
    );

    // Performance
    method(reg, rt, "Performance", "now", |cx, w| cx.ret(coarse_now(w)));
    getter(reg, rt, "Performance", "timeOrigin", |cx, w| {
        cx.ret((w.clock.time_origin * 10.0).floor() / 10.0)
    });
    method(reg, rt, "Performance", "toJSON", |cx, w| {
        cx.ret(Arg::Json(
            serde_json::json!({ "timeOrigin": (w.clock.time_origin * 10.0).floor() / 10.0 })
                .to_string(),
        ))
    });
    for name in ["getEntries", "getEntriesByType", "getEntriesByName"] {
        method(reg, rt, "Performance", name, |cx, _w| {
            cx.ret(Arg::Array(Vec::new()))
        });
    }
    for name in [
        "clearMarks",
        "clearMeasures",
        "clearResourceTimings",
        "setResourceTimingBufferSize",
    ] {
        method(reg, rt, "Performance", name, |_cx, _w| Ok(()));
    }

    // Screen
    macro_rules! scr {
        ($name:literal, |$s:ident| $e:expr) => {
            getter(reg, rt, "Screen", $name, |cx, w| {
                let $s = screen_dims(w);
                cx.ret($e)
            });
        };
    }
    // Sizes of a removed frame's screen read 0 (Chrome); depths stay.
    macro_rules! scr_size {
        ($name:literal, |$s:ident| $e:expr) => {
            getter(reg, rt, "Screen", $name, |cx, w| {
                if super::frames::is_detached(w) {
                    return cx.ret(0);
                }
                let $s = screen_dims(w);
                cx.ret($e)
            });
        };
    }
    scr_size!("width", |s| s.width);
    scr_size!("height", |s| s.height);
    scr_size!("availWidth", |s| s.avail_width);
    scr_size!("availHeight", |s| s.avail_height);
    scr_size!("availLeft", |s| s.avail_left);
    scr_size!("availTop", |s| s.avail_top);
    scr!("colorDepth", |s| s.color_depth);
    scr!("pixelDepth", |s| s.pixel_depth);
    getter(reg, rt, "Screen", "isExtended", |cx, _w| cx.ret(false));
}

fn register_navigator(reg: &mut Registry, rt: &Runtime) {
    macro_rules! nav {
        ($name:literal, |$w:ident| $e:expr) => {
            getter(reg, rt, "Navigator", $name, |cx, $w| {
                this_obj(cx)?;
                cx.ret($e)
            });
        };
    }
    // A removed frame's navigator has no user agent (Chrome: "").
    nav!("userAgent", |w| if super::frames::is_detached(w) {
        String::new()
    } else {
        w.persona.user_agent.clone()
    });
    nav!("appVersion", |w| match &w.profile {
        _ if super::frames::is_detached(w) => String::new(),
        Some(p) => p.browser.app_version.clone(),
        None => w
            .persona
            .user_agent
            .trim_start_matches("Mozilla/")
            .to_owned(),
    });
    nav!("platform", |w| match &w.profile {
        Some(p) => p.os.platform.clone(),
        None => platform_from_ua(&w.persona.user_agent).to_owned(),
    });
    nav!("language", |w| languages(w)
        .into_iter()
        .next()
        .unwrap_or_else(|| "en-US".into()));
    nav!("vendor", |w| w.profile.as_ref().map_or_else(
        || "Google Inc.".to_owned(),
        |p| p.browser.vendor.clone()
    ));
    nav!("vendorSub", |w| w
        .profile
        .as_ref()
        .map_or_else(String::new, |p| p.browser.vendor_sub.clone()));
    nav!("product", |w| w.profile.as_ref().map_or_else(
        || "Gecko".to_owned(),
        |p| p.browser.product.clone()
    ));
    nav!("productSub", |w| w.profile.as_ref().map_or_else(
        || "20030107".to_owned(),
        |p| p.browser.product_sub.clone()
    ));
    nav!("appName", |w| w.profile.as_ref().map_or_else(
        || "Netscape".to_owned(),
        |p| p.browser.app_name.clone()
    ));
    nav!("appCodeName", |w| w.profile.as_ref().map_or_else(
        || "Mozilla".to_owned(),
        |p| p.browser.app_code_name.clone()
    ));
    nav!("cookieEnabled", |w| !super::frames::is_detached(w)
        && w.profile
            .as_ref()
            .is_none_or(|p| p.browser.cookie_enabled));
    nav!("onLine", |_w| true);
    nav!("webdriver", |_w| false);
    nav!("pdfViewerEnabled", |w| w
        .profile
        .as_ref()
        .is_none_or(|p| p.browser.pdf_viewer_enabled));
    nav!("hardwareConcurrency", |w| w
        .profile
        .as_ref()
        .map_or(8, |p| p.hardware.hardware_concurrency));
    nav!("deviceMemory", |w| w
        .profile
        .as_ref()
        .map_or(8.0, |p| p.hardware.device_memory));
    nav!("maxTouchPoints", |w| w
        .profile
        .as_ref()
        .map_or(0, |p| p.hardware.max_touch_points));
    getter(reg, rt, "Navigator", "doNotTrack", |cx, w| {
        this_obj(cx)?;
        match w
            .profile
            .as_ref()
            .and_then(|p| p.browser.do_not_track.clone())
        {
            Some(v) => cx.ret(v),
            None => cx.ret(Arg::Null),
        }
    });
    method(reg, rt, "Navigator", "javaEnabled", |cx, _w| cx.ret(false));
    getter(reg, rt, "Navigator", "languages", |cx, w| {
        this_obj(cx)?;
        if let Some(h) = w.helpers.borrow().languages {
            return cx.ret(Arg::Handle(h));
        }
        let json = serde_json::to_string(&languages(w)).unwrap_or_else(|_| "[]".into());
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
        w.helpers.borrow_mut().languages = Some(arr);
        cx.ret(Arg::Handle(arr))
    });
}

fn languages(w: &Window) -> Vec<String> {
    if let Some(p) = &w.profile
        && !p.locale.languages.is_empty()
    {
        return p.locale.languages.clone();
    }
    w.persona
        .accept_language
        .split(',')
        .map(|l| l.split(';').next().unwrap_or("").trim().to_owned())
        .filter(|l| !l.is_empty())
        .collect()
}

fn platform_from_ua(ua: &str) -> &'static str {
    if ua.contains("Windows") {
        "Win32"
    } else if ua.contains("Mac OS X") {
        "MacIntel"
    } else if ua.contains("Android") {
        "Linux armv81"
    } else {
        "Linux x86_64"
    }
}

impl Window {
    fn helpers_name(&self) -> String {
        self.window_name.borrow().clone()
    }
    fn set_helpers_name(&self, v: String) {
        *self.window_name.borrow_mut() = v;
    }
}

#[allow(dead_code)]
fn unused(_: HostRef) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trip() {
        for s in [&b""[..], b"a", b"ab", b"abc", b"abcd", b"\xff\x00\x10"] {
            assert_eq!(base64_decode(&base64_encode(s)).as_deref(), Some(s));
        }
        assert_eq!(base64_encode(b"hello"), "aGVsbG8=");
        assert_eq!(base64_decode("aGVsbG8").as_deref(), Some(&b"hello"[..]));
        assert_eq!(base64_decode("a"), None);
        assert_eq!(base64_decode("a$=="), None);
    }

    #[test]
    fn dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_000), (2024, 10, 4));
    }
}

//! XMLHttpRequest, fetch, Headers, Request, Response, AbortController and
//! AbortSignal over the page layer's [`crate::host::NetHost`].
//!
//! Requests go out through the context's network stack (persona headers,
//! cookie jar, redirects). Responses complete in a networking task. CORS:
//! a cross-origin response must carry a matching
//! `Access-Control-Allow-Origin` (no preflight yet).

use std::cell::RefCell;
use std::rc::Rc;

use lb_js::{Arg, CallCtx, Handle, HostError, HostRef, Resolver, ValueKind};
use url::Url;

use super::events::{EventExtra, Listener, fire_event, fire_event_with, remove_listener};
use super::{
    Dict, Js, K_OBJ, Obj, Runtime, TaskSource, TimerCallback, Window, ctor, getter, illegal,
    method, need_args, opt_string, setter, this_obj,
};
use crate::host::{Destination, NetRequest, NetResponse, decode_text};
use crate::registry::{ImplKind, Registry};

// ---------------------------------------------------------------------------
// Abort
// ---------------------------------------------------------------------------

/// What happens when a signal aborts.
pub(crate) enum AbortAlgo {
    RemoveListener(HostRef, Rc<Listener>),
    /// A pending `fetch` promise (shared with the completion task).
    RejectFetch(Rc<RefCell<Option<Resolver>>>),
    /// A dependent signal (`AbortSignal.any`).
    Dependent(u32),
}

#[derive(Default)]
pub(crate) struct SignalData {
    pub aborted: bool,
    pub reason: Option<Handle>,
    algos: Vec<AbortAlgo>,
}

pub(crate) fn signal_aborted(w: &Window, sig: u32) -> bool {
    w.with_obj(sig, |o| matches!(o, Obj::AbortSignal(s) if s.aborted))
        .unwrap_or(false)
}

pub(crate) fn signal_on_abort(w: &Window, sig: u32, algo: AbortAlgo) {
    w.with_obj(sig, |o| {
        if let Obj::AbortSignal(s) = o {
            s.algos.push(algo);
        }
    });
}

fn signal_reason(w: &Window, sig: u32) -> Option<Handle> {
    w.with_obj(sig, |o| match o {
        Obj::AbortSignal(s) => s.reason,
        _ => None,
    })
    .flatten()
}

fn dom_exception(w: &Window, name: &str, msg: &str) -> Arg {
    match w.rt.dom().interface() {
        Some(id) => Arg::Construct(id, vec![Arg::String(msg.into()), Arg::String(name.into())]),
        None => Arg::String(msg.into()),
    }
}

/// Signal abort: set the reason, run the abort algorithms, fire `abort`.
pub(crate) fn abort_signal(js: &mut dyn Js, w: &Rc<Window>, sig: u32, reason: Option<Handle>) {
    if signal_aborted(w, sig) {
        return;
    }
    let reason = reason.or_else(|| {
        js.make(dom_exception(
            w,
            "AbortError",
            "signal is aborted without reason",
        ))
    });
    let algos = w
        .with_obj(sig, |o| match o {
            Obj::AbortSignal(s) => {
                s.aborted = true;
                s.reason = reason;
                std::mem::take(&mut s.algos)
            }
            _ => Vec::new(),
        })
        .unwrap_or_default();
    let mut dependents = Vec::new();
    for a in algos {
        match a {
            AbortAlgo::RemoveListener(t, l) => remove_listener(w, t, &l),
            AbortAlgo::RejectFetch(p) => {
                if let Some(r) = p.borrow_mut().take() {
                    let e = match reason {
                        Some(h) => HostError::Throw(Arg::Handle(h)),
                        None => HostError::Error("AbortError".into()),
                    };
                    js.reject(r, e);
                }
            }
            AbortAlgo::Dependent(d) => dependents.push(d),
        }
    }
    fire_event(js, w, HostRef::new(K_OBJ, sig), "abort", false, false);
    for d in dependents {
        abort_signal(js, w, d, reason);
    }
}

fn new_signal(w: &Window) -> u32 {
    w.new_obj(Obj::AbortSignal(Box::default()))
}

fn this_signal(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<u32, HostError> {
    let id = this_obj(cx)?;
    match w.with_obj(id, |o| matches!(o, Obj::AbortSignal(_))) {
        Some(true) => Ok(id),
        _ => Err(illegal()),
    }
}

// ---------------------------------------------------------------------------
// Headers
// ---------------------------------------------------------------------------

#[derive(Default, Clone)]
pub(crate) struct HeadersData {
    /// Lowercased names, in insertion order.
    pub list: Vec<(String, String)>,
    pub immutable: bool,
}

impl HeadersData {
    fn append(&mut self, n: &str, v: &str) {
        self.list.push((n.to_ascii_lowercase(), normalize_value(v)));
    }
    fn set(&mut self, n: &str, v: &str) {
        let n = n.to_ascii_lowercase();
        let v = normalize_value(v);
        match self.list.iter().position(|(k, _)| *k == n) {
            Some(i) => {
                self.list[i].1 = v;
                let mut j = i + 1;
                while j < self.list.len() {
                    if self.list[j].0 == n {
                        self.list.remove(j);
                    } else {
                        j += 1;
                    }
                }
            }
            None => self.list.push((n, v)),
        }
    }
    fn get(&self, n: &str) -> Option<String> {
        let n = n.to_ascii_lowercase();
        let vals: Vec<&str> = self
            .list
            .iter()
            .filter(|(k, _)| *k == n)
            .map(|(_, v)| v.as_str())
            .collect();
        (!vals.is_empty()).then(|| vals.join(", "))
    }
    /// Sorted and combined (iteration order); `set-cookie` stays separate.
    fn sorted(&self) -> Vec<(String, String)> {
        let mut names: Vec<&String> = self.list.iter().map(|(k, _)| k).collect();
        names.sort();
        names.dedup();
        let mut out = Vec::new();
        for n in names {
            if n == "set-cookie" {
                for (k, v) in &self.list {
                    if k == n {
                        out.push((k.clone(), v.clone()));
                    }
                }
            } else if let Some(v) = self.get(n) {
                out.push((n.clone(), v));
            }
        }
        out
    }
}

fn normalize_value(v: &str) -> String {
    v.trim_matches(|c| matches!(c, ' ' | '\t' | '\n' | '\r'))
        .to_owned()
}

fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && n.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

fn headers_of(w: &Window, id: u32) -> Option<HeadersData> {
    w.with_obj(id, |o| match o {
        Obj::Headers(h) => Some((**h).clone()),
        _ => None,
    })
    .flatten()
}

/// Fill `h` from a `HeadersInit` argument.
fn fill_headers(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    i: usize,
    h: &mut HeadersData,
) -> Result<(), HostError> {
    if i >= cx.len() || matches!(cx.arg_kind(i), ValueKind::Undefined | ValueKind::Null) {
        return Ok(());
    }
    if let Some(src) = cx
        .arg_host(i)
        .filter(|r| r.kind == K_OBJ)
        .and_then(|r| headers_of(w, r.id))
    {
        for (k, v) in src.list {
            h.append(&k, &v);
        }
        return Ok(());
    }
    let v = cx.arg_value(i);
    let json = match v {
        lb_js::JsValue::Json(s) => s,
        _ => {
            return Err(HostError::type_error(
                "Failed to construct 'Headers': The provided value is not of type '(record<ByteString, ByteString> or sequence<sequence<ByteString>>)'.",
            ));
        }
    };
    match serde_json::from_str::<serde_json::Value>(&json) {
        Ok(serde_json::Value::Array(items)) => {
            for it in items {
                let pair = it.as_array().cloned().unwrap_or_default();
                if pair.len() != 2 {
                    return Err(HostError::type_error(
                        "Failed to construct 'Headers': Invalid value",
                    ));
                }
                h.append(&json_str(&pair[0]), &json_str(&pair[1]));
            }
        }
        Ok(serde_json::Value::Object(m)) => {
            for (k, v) in m {
                if !valid_name(&k) {
                    return Err(HostError::type_error(
                        "Failed to construct 'Headers': Invalid name",
                    ));
                }
                h.append(&k, &json_str(&v));
            }
        }
        _ => {}
    }
    Ok(())
}

fn json_str(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Bodies
// ---------------------------------------------------------------------------

/// Body bytes and implied `Content-Type` of a `BodyInit` argument.
fn extract_body(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    i: usize,
) -> Result<Option<(Vec<u8>, Option<String>)>, HostError> {
    if i >= cx.len() || matches!(cx.arg_kind(i), ValueKind::Undefined | ValueKind::Null) {
        return Ok(None);
    }
    if let Some(b) = cx.arg_bytes(i) {
        return Ok(Some((b, None)));
    }
    if let Some(r) = cx.arg_host(i).filter(|r| r.kind == K_OBJ) {
        let found = w.with_obj(r.id, |o| match o {
            Obj::Params(p) => Some((
                super::misc::serialize_params(&p.list).into_bytes(),
                Some("application/x-www-form-urlencoded;charset=UTF-8".to_owned()),
            )),
            Obj::Blob(b) => Some((
                b.bytes.clone(),
                (!b.type_.is_empty()).then(|| b.type_.clone()),
            )),
            _ => None,
        });
        if let Some(Some(x)) = found {
            return Ok(Some(x));
        }
    }
    let s = cx.arg_string(i)?;
    Ok(Some((
        s.into_bytes(),
        Some("text/plain;charset=UTF-8".to_owned()),
    )))
}

pub(crate) struct ResponseData {
    pub status: u16,
    pub status_text: String,
    pub headers: u32,
    pub body: Option<Vec<u8>>,
    pub used: bool,
    pub url: String,
    pub redirected: bool,
    pub type_: &'static str,
}

pub(crate) struct RequestData {
    pub method: String,
    pub url: Url,
    pub headers: u32,
    pub body: Option<Vec<u8>>,
    pub used: bool,
    pub signal: u32,
    pub mode: &'static str,
    pub credentials: String,
    pub cache: String,
    pub redirect: String,
}

fn new_headers(w: &Window, h: HeadersData) -> u32 {
    w.new_obj(Obj::Headers(Box::new(h)))
}

/// Take the body of a Request/Response for a body-reading method.
fn take_body(w: &Window, id: u32, op: &str) -> Result<(Vec<u8>, Option<String>), HostError> {
    let iface = w.with_obj(id, |o| o.iface_name()).unwrap_or("Response");
    let r = w.with_obj(id, |o| match o {
        Obj::Response(r) => {
            if r.used {
                return Err(());
            }
            r.used = r.body.is_some();
            Ok((r.body.clone().unwrap_or_default(), r.headers))
        }
        Obj::Request(r) => {
            if r.used {
                return Err(());
            }
            r.used = r.body.is_some();
            Ok((r.body.clone().unwrap_or_default(), r.headers))
        }
        _ => Err(()),
    });
    match r {
        Some(Ok((b, h))) => {
            let ct = headers_of(w, h).and_then(|h| h.get("content-type"));
            Ok((b, ct))
        }
        _ => Err(HostError::type_error(format!(
            "Failed to execute '{op}' on '{iface}': body stream already read"
        ))),
    }
}

fn body_method(reg: &mut Registry, rt: &Runtime, iface: &'static str, name: &'static str) {
    method(reg, rt, iface, name, move |cx, w| {
        let id = this_obj(cx)?;
        let body = take_body(w, id, name);
        let r = cx.ret_new_promise()?;
        let (bytes, ct) = match body {
            Ok(b) => b,
            Err(e) => return cx.reject_promise(r, e),
        };
        match name {
            "text" => {
                let (t, _) = decode_text(&bytes, Some("text/plain;charset=utf-8"), false);
                cx.resolve_promise(r, Arg::String(t))
            }
            "json" => {
                let (t, _) = decode_text(&bytes, Some("text/plain;charset=utf-8"), false);
                match serde_json::from_str::<serde_json::Value>(&t) {
                    Ok(_) => cx.resolve_promise(r, Arg::Json(t)),
                    Err(_) => {
                        let msg = if t.is_empty() {
                            "Unexpected end of JSON input".to_owned()
                        } else {
                            format!(
                                "Unexpected token '{}', \"{}\" is not valid JSON",
                                t.chars().next().unwrap_or(' '),
                                t.chars().take(30).collect::<String>()
                            )
                        };
                        cx.reject_promise(r, HostError::SyntaxError(msg))
                    }
                }
            }
            "arrayBuffer" => cx.resolve_promise(r, Arg::ArrayBuffer(bytes)),
            "bytes" => cx.resolve_promise(r, Arg::Uint8Array(bytes)),
            "blob" => {
                let b = w.new_obj(Obj::Blob(Box::new(super::misc::BlobData {
                    bytes,
                    type_: ct.unwrap_or_default().to_ascii_lowercase(),
                })));
                let a = w.obj_arg(b);
                cx.resolve_promise(r, a)
            }
            _ => cx.reject_promise(
                r,
                HostError::type_error(format!(
                    "Failed to execute '{name}' on '{iface}': not supported"
                )),
            ),
        }
    });
}

/// CORS check for a cross-origin response.
fn cors_ok(origin: &url::Origin, resp: &NetResponse) -> bool {
    if resp.url.origin() == *origin {
        return true;
    }
    match resp.header("access-control-allow-origin") {
        Some("*") => true,
        Some(v) => v.trim() == origin.ascii_serialization(),
        None => false,
    }
}

/// Response headers visible to script (forbidden response header names
/// are filtered).
fn visible_headers(resp: &NetResponse, cors: bool) -> HeadersData {
    let mut h = HeadersData::default();
    let safelisted = [
        "cache-control",
        "content-language",
        "content-length",
        "content-type",
        "expires",
        "last-modified",
        "pragma",
    ];
    let exposed: Vec<String> = resp
        .header("access-control-expose-headers")
        .map(|v| {
            v.split(',')
                .map(|s| s.trim().to_ascii_lowercase())
                .collect()
        })
        .unwrap_or_default();
    for (k, v) in &resp.headers {
        let lk = k.to_ascii_lowercase();
        if lk == "set-cookie" || lk == "set-cookie2" {
            continue;
        }
        if cors
            && !safelisted.contains(&lk.as_str())
            && !exposed.contains(&lk)
            && !exposed.iter().any(|e| e == "*")
        {
            continue;
        }
        h.append(&lk, v);
    }
    h
}

// ---------------------------------------------------------------------------
// fetch
// ---------------------------------------------------------------------------

/// Start `req` and run `done` in a networking task with the result.
pub(crate) fn start_request(
    w: &Rc<Window>,
    req: NetRequest,
    done: Box<dyn FnOnce(&mut dyn Js, &Rc<Window>, Result<NetResponse, String>)>,
) {
    w.in_flight.set(w.in_flight.get() + 1);
    let fut = w.net.fetch(req);
    let weak = Rc::downgrade(w);
    w.net.spawn(Box::pin(async move {
        let r = fut.await;
        if let Some(w) = weak.upgrade() {
            w.in_flight.set(w.in_flight.get().saturating_sub(1));
            if w.is_closed() {
                return;
            }
            w.queue_task(
                TaskSource::Networking,
                Box::new(move |js, w| done(js, w, r)),
            );
        }
    }));
}

fn do_fetch(cx: &mut CallCtx<'_, '_, '_>, w: &Rc<Window>) -> Result<(), HostError> {
    // Build a Request from the arguments (same rules as `new Request`).
    let req_id = build_request(cx, w, "fetch", "Window")?;
    let Some(Some((method, url, headers, body, signal, mode))) = w.with_obj(req_id, |o| match o {
        Obj::Request(r) => Some((
            r.method.clone(),
            r.url.clone(),
            r.headers,
            r.body.clone(),
            r.signal,
            r.mode,
        )),
        _ => None,
    }) else {
        return Err(illegal());
    };
    let r = cx.ret_new_promise()?;
    if signal_aborted(w, signal) {
        let reason = signal_reason(w, signal);
        return cx.reject_promise(
            r,
            reason.map_or(HostError::Error("AbortError".into()), |h| {
                HostError::Throw(Arg::Handle(h))
            }),
        );
    }
    if !matches!(url.scheme(), "http" | "https") {
        return cx.reject_promise(r, HostError::type_error("Failed to fetch"));
    }
    let origin = w.url().origin();
    if mode == "same-origin" && url.origin() != origin {
        return cx.reject_promise(r, HostError::type_error("Failed to fetch"));
    }
    let pending = Rc::new(RefCell::new(Some(r)));
    signal_on_abort(w, signal, AbortAlgo::RejectFetch(pending.clone()));
    let req = NetRequest {
        method,
        url: url.clone(),
        headers: headers_of(w, headers).map(|h| h.list).unwrap_or_default(),
        body: body.unwrap_or_default(),
        destination: Destination::Empty,
        initiator: Some(w.url()),
        mode,
    };
    start_request(
        w,
        req,
        Box::new(move |js, w, result| {
            let Some(r) = pending.borrow_mut().take() else {
                return;
            };
            match result {
                Ok(resp) if mode == "no-cors" || cors_ok(&origin, &resp) => {
                    let cors = resp.url.origin() != origin;
                    let h = new_headers(
                        w,
                        HeadersData {
                            list: visible_headers(&resp, cors).list,
                            immutable: true,
                        },
                    );
                    let id = w.new_obj(Obj::Response(Box::new(ResponseData {
                        status: resp.status,
                        status_text: resp.status_text.clone(),
                        headers: h,
                        body: Some(resp.body),
                        used: false,
                        url: resp.url.to_string(),
                        redirected: resp.redirected,
                        type_: if cors { "cors" } else { "basic" },
                    })));
                    let a = w.obj_arg(id);
                    js.resolve(r, a);
                }
                Ok(_) => {
                    w.log(crate::host::ConsoleMessage { level: "error".into(), text: format!("Access to fetch at '{url}' from origin '{}' has been blocked by CORS policy: No 'Access-Control-Allow-Origin' header is present on the requested resource.", origin.ascii_serialization()), url: None, line: None });
                    js.reject(r, HostError::type_error("Failed to fetch"));
                }
                Err(_) => js.reject(r, HostError::type_error("Failed to fetch")),
            }
        }),
    );
    Ok(())
}

/// `new Request(input, init)` / the first step of `fetch`.
fn build_request(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Rc<Window>,
    op: &str,
    iface: &str,
) -> Result<u32, HostError> {
    need_args(cx, 1, op, iface)?;
    let mut base: Option<(
        String,
        Url,
        HeadersData,
        Option<Vec<u8>>,
        &'static str,
        String,
    )> = None;
    if let Some(h) = cx.arg_host(0).filter(|h| h.kind == K_OBJ) {
        base = w
            .with_obj(h.id, |o| match o {
                Obj::Request(r) => Some((
                    r.method.clone(),
                    r.url.clone(),
                    r.headers,
                    r.body.clone(),
                    r.mode,
                    r.credentials.clone(),
                )),
                _ => None,
            })
            .flatten()
            .map(|(m, u, hid, b, mode, cred)| {
                (m, u, headers_of(w, hid).unwrap_or_default(), b, mode, cred)
            });
    }
    let (mut method, url, mut headers, mut body, mut mode, mut credentials) = match base {
        Some(b) => b,
        None => {
            let s = cx.arg_string(0)?;
            let u = w.resolve_url(&s).ok_or_else(|| {
                HostError::type_error(format!(
                    "Failed to execute '{op}' on '{iface}': Failed to parse URL from {s}"
                ))
            })?;
            if !u.username().is_empty() || u.password().is_some() {
                return Err(HostError::type_error(format!(
                    "Failed to execute '{op}' on '{iface}': Request cannot be constructed from a URL that includes credentials: {s}"
                )));
            }
            (
                "GET".to_owned(),
                u,
                HeadersData::default(),
                None,
                "cors",
                "same-origin".to_owned(),
            )
        }
    };
    let init = Dict::arg(cx, 1);
    let mut signal_src = None;
    let (mut cache, mut redirect) = ("default".to_owned(), "follow".to_owned());
    if init.handle().is_some() {
        if let Some(m) = init.string(cx, "method")? {
            let up = m.to_ascii_uppercase();
            if matches!(up.as_str(), "CONNECT" | "TRACE" | "TRACK") {
                init.release(cx);
                return Err(HostError::type_error(format!(
                    "Failed to execute '{op}' on '{iface}': '{m}' HTTP method is unsupported."
                )));
            }
            method = if matches!(
                up.as_str(),
                "DELETE" | "GET" | "HEAD" | "OPTIONS" | "POST" | "PUT"
            ) {
                up
            } else {
                m
            };
        }
        if let Some(hh) = init.get(cx, "headers")? {
            let mut h = HeadersData::default();
            // Reuse the HeadersInit conversion through a temporary arg list.
            let host = cx
                .handle_host(hh)
                .filter(|r| r.kind == K_OBJ)
                .and_then(|r| headers_of(w, r.id));
            match host {
                Some(src) => h = src,
                None => {
                    if let lb_js::JsValue::Json(s) = cx.handle_value(hh) {
                        match serde_json::from_str::<serde_json::Value>(&s) {
                            Ok(serde_json::Value::Array(items)) => {
                                for it in items {
                                    if let Some(p) = it.as_array().filter(|p| p.len() == 2) {
                                        h.append(&json_str(&p[0]), &json_str(&p[1]));
                                    }
                                }
                            }
                            Ok(serde_json::Value::Object(m)) => {
                                for (k, v) in m {
                                    h.append(&k, &json_str(&v));
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            h.immutable = false;
            headers = h;
            cx.release(hh);
        }
        if let Some(bh) = init.get(cx, "body")? {
            if matches!(method.as_str(), "GET" | "HEAD") && cx.handle_kind(bh) != ValueKind::Null {
                cx.release(bh);
                init.release(cx);
                return Err(HostError::type_error(format!(
                    "Failed to execute '{op}' on '{iface}': Request with GET/HEAD method cannot have body."
                )));
            }
            let b = body_from_handle(cx, w, bh)?;
            cx.release(bh);
            if let Some((bytes, ct)) = b {
                if let Some(ct) = ct
                    && headers.get("content-type").is_none()
                {
                    headers.append("content-type", &ct);
                }
                body = Some(bytes);
            }
        }
        if let Some(m) = init.string(cx, "mode")? {
            mode = match m.as_str() {
                "same-origin" => "same-origin",
                "no-cors" => "no-cors",
                "navigate" => {
                    init.release(cx);
                    return Err(HostError::type_error(format!(
                        "Failed to execute '{op}' on '{iface}': Cannot construct a Request with a RequestInit whose mode member is set as 'navigate'."
                    )));
                }
                _ => "cors",
            };
        }
        if let Some(c) = init.string(cx, "credentials")? {
            credentials = c;
        }
        if let Some(c) = init.string(cx, "cache")? {
            cache = c;
        }
        if let Some(c) = init.string(cx, "redirect")? {
            redirect = c;
        }
        if let Some(sh) = init.get(cx, "signal")? {
            signal_src = cx.handle_host(sh).filter(|r| r.kind == K_OBJ).map(|r| r.id);
            cx.release(sh);
        }
    }
    init.release(cx);
    let signal = new_signal(w);
    if let Some(src) = signal_src {
        if signal_aborted(w, src) {
            let reason = signal_reason(w, src);
            w.with_obj(signal, |o| {
                if let Obj::AbortSignal(s) = o {
                    s.aborted = true;
                    s.reason = reason;
                }
            });
        } else {
            signal_on_abort(w, src, AbortAlgo::Dependent(signal));
        }
    }
    let hid = new_headers(w, headers);
    Ok(w.new_obj(Obj::Request(Box::new(RequestData {
        method,
        url,
        headers: hid,
        body,
        used: false,
        signal,
        mode,
        credentials,
        cache,
        redirect,
    }))))
}

fn body_from_handle(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    h: Handle,
) -> Result<Option<(Vec<u8>, Option<String>)>, HostError> {
    match cx.handle_kind(h) {
        ValueKind::Undefined | ValueKind::Null => return Ok(None),
        _ => {}
    }
    if let Some(b) = cx.handle_bytes(h) {
        return Ok(Some((b, None)));
    }
    if let Some(r) = cx.handle_host(h).filter(|r| r.kind == K_OBJ) {
        let found = w.with_obj(r.id, |o| match o {
            Obj::Params(p) => Some((
                super::misc::serialize_params(&p.list).into_bytes(),
                Some("application/x-www-form-urlencoded;charset=UTF-8".to_owned()),
            )),
            Obj::Blob(b) => Some((
                b.bytes.clone(),
                (!b.type_.is_empty()).then(|| b.type_.clone()),
            )),
            _ => None,
        });
        if let Some(Some(x)) = found {
            return Ok(Some(x));
        }
    }
    let s = cx.handle_string(h)?;
    Ok(Some((
        s.into_bytes(),
        Some("text/plain;charset=UTF-8".to_owned()),
    )))
}

// ---------------------------------------------------------------------------
// XMLHttpRequest
// ---------------------------------------------------------------------------

#[derive(Default)]
pub(crate) struct XhrState {
    method: String,
    url: Option<Url>,
    headers: Vec<(String, String)>,
    is_async: bool,
    pub ready_state: u16,
    send_flag: bool,
    status: u16,
    status_text: String,
    resp_headers: Vec<(String, String)>,
    body: Vec<u8>,
    response_url: String,
    response_type: String,
    timeout: u32,
    with_credentials: bool,
    /// Bumped by `open`/`abort` so stale completions are dropped.
    generation: u32,
    error: bool,
    json: Option<Handle>,
    mime_override: Option<String>,
    upload: Option<u32>,
}

fn this_xhr(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<u32, HostError> {
    let id = this_obj(cx)?;
    match w.with_obj(id, |o| matches!(o, Obj::Xhr(_))) {
        Some(true) => Ok(id),
        _ => Err(illegal()),
    }
}

fn with_xhr<R>(w: &Window, id: u32, f: impl FnOnce(&mut XhrState) -> R) -> Option<R> {
    w.with_obj(id, |o| match o {
        Obj::Xhr(x) => Some(f(x)),
        _ => None,
    })
    .flatten()
}

fn xhr_event(js: &mut dyn Js, w: &Rc<Window>, id: u32, type_: &str, loaded: f64, total: f64) {
    let extra = EventExtra::Progress {
        computable: total > 0.0,
        loaded,
        total,
    };
    fire_event_with(
        js,
        w,
        HostRef::new(K_OBJ, id),
        "ProgressEvent",
        type_,
        false,
        false,
        extra,
    );
}

fn xhr_rsc(js: &mut dyn Js, w: &Rc<Window>, id: u32) {
    fire_event(
        js,
        w,
        HostRef::new(K_OBJ, id),
        "readystatechange",
        false,
        false,
    );
}

fn xhr_complete(
    js: &mut dyn Js,
    w: &Rc<Window>,
    id: u32,
    generation: u32,
    result: Result<NetResponse, String>,
    origin: url::Origin,
) {
    let Some(true) = with_xhr(w, id, |x| x.generation == generation && x.send_flag) else {
        return;
    };
    match result {
        Ok(resp) if cors_ok(&origin, &resp) => {
            let cors = resp.url.origin() != origin;
            let len = resp.body.len() as f64;
            let total = resp
                .header("content-length")
                .and_then(|v| v.trim().parse::<f64>().ok())
                .unwrap_or(0.0);
            with_xhr(w, id, |x| {
                x.status = resp.status;
                x.status_text = resp.status_text.clone();
                x.resp_headers = visible_headers(&resp, cors).list;
                x.response_url = resp.url.to_string();
                x.ready_state = 2;
            });
            xhr_rsc(js, w, id);
            if with_xhr(w, id, |x| x.generation != generation).unwrap_or(true) {
                return;
            }
            with_xhr(w, id, |x| {
                x.body = resp.body;
                x.ready_state = 3;
            });
            xhr_rsc(js, w, id);
            xhr_event(js, w, id, "progress", len, total);
            if with_xhr(w, id, |x| x.generation != generation).unwrap_or(true) {
                return;
            }
            with_xhr(w, id, |x| {
                x.ready_state = 4;
                x.send_flag = false;
            });
            xhr_rsc(js, w, id);
            xhr_event(js, w, id, "load", len, total);
            xhr_event(js, w, id, "loadend", len, total);
        }
        other => {
            if other.is_ok() {
                w.log(crate::host::ConsoleMessage { level: "error".into(), text: format!("Access to XMLHttpRequest from origin '{}' has been blocked by CORS policy: No 'Access-Control-Allow-Origin' header is present on the requested resource.", origin.ascii_serialization()), url: None, line: None });
            }
            with_xhr(w, id, |x| {
                x.ready_state = 4;
                x.send_flag = false;
                x.error = true;
                x.status = 0;
                x.body.clear();
                x.resp_headers.clear();
            });
            xhr_rsc(js, w, id);
            xhr_event(js, w, id, "error", 0.0, 0.0);
            xhr_event(js, w, id, "loadend", 0.0, 0.0);
        }
    }
}

fn xhr_text(x: &XhrState) -> String {
    let ct = x.mime_override.clone().or_else(|| {
        x.resp_headers
            .iter()
            .find(|(k, _)| k == "content-type")
            .map(|(_, v)| v.clone())
    });
    let ct = ct.map(|c| {
        if crate::host::charset_param(&c).is_some() {
            c
        } else {
            format!("{c};charset=utf-8")
        }
    });
    decode_text(
        &x.body,
        ct.as_deref().or(Some("text/plain;charset=utf-8")),
        false,
    )
    .0
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Chrome's keepalive quota for one beacon (bytes).
const BEACON_QUOTA: usize = 64 * 1024;

/// `navigator.sendBeacon(url, data)`: a keepalive POST that outlives the
/// document (it is sent even from `pagehide`). Returns `false` when the
/// body is over the quota, like Chrome; the response is never exposed.
fn send_beacon(cx: &mut CallCtx<'_, '_, '_>, w: &Rc<Window>) -> Result<(), HostError> {
    need_args(cx, 1, "sendBeacon", "Navigator")?;
    let s = cx.arg_string(0)?;
    let url = w.resolve_url(&s).ok_or_else(|| {
        HostError::type_error(
            "Failed to execute 'sendBeacon' on 'Navigator': The URL argument is ill-formed or unsupported.",
        )
    })?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(HostError::type_error(
            "Failed to execute 'sendBeacon' on 'Navigator': Beacons are only supported over HTTP(S).",
        ));
    }
    let body = if cx.len() > 1 {
        let h = cx.arg_handle(1);
        let b = body_from_handle(cx, w, h)?;
        cx.release(h);
        b
    } else {
        None
    };
    let (body, ct) = body.unwrap_or_default();
    if body.len() > BEACON_QUOTA {
        return cx.ret(false);
    }
    // A content type outside the CORS-safelisted ones makes it a cors request.
    let safelisted = ct.as_deref().is_none_or(|t| {
        let mime = t
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        matches!(
            mime.as_str(),
            "text/plain" | "application/x-www-form-urlencoded" | "multipart/form-data"
        )
    });
    let req = NetRequest {
        method: "POST".to_owned(),
        url,
        headers: ct
            .map(|t| vec![("Content-Type".to_owned(), t)])
            .unwrap_or_default(),
        body,
        destination: Destination::Empty,
        initiator: Some(w.url()),
        mode: if safelisted { "no-cors" } else { "cors" },
    };
    // Not tied to the window: the request survives the document's unload.
    let fut = w.net.fetch(req);
    w.net.spawn(Box::pin(async move {
        let _ = fut.await;
    }));
    cx.ret(true)
}

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    method(reg, rt, "Navigator", "sendBeacon", |cx, w| {
        send_beacon(cx, w)
    });
    // AbortController / AbortSignal
    ctor(reg, rt, "AbortController", |cx, w| {
        let sig = new_signal(w);
        let id = w.new_obj(Obj::AbortController { signal: sig });
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    fn ctrl_signal(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<u32, HostError> {
        let id = this_obj(cx)?;
        w.with_obj(id, |o| match o {
            Obj::AbortController { signal } => Some(*signal),
            _ => None,
        })
        .flatten()
        .ok_or_else(illegal)
    }
    getter(reg, rt, "AbortController", "signal", |cx, w| {
        let s = ctrl_signal(cx, w)?;
        cx.ret(w.obj_arg(s))
    });
    method(reg, rt, "AbortController", "abort", |cx, w| {
        let s = ctrl_signal(cx, w)?;
        let reason =
            (!cx.is_empty() && cx.arg_kind(0) != ValueKind::Undefined).then(|| cx.arg_handle(0));
        abort_signal(cx, w, s, reason);
        w.after_mutation(cx);
        Ok(())
    });
    getter(reg, rt, "AbortSignal", "aborted", |cx, w| {
        let s = this_signal(cx, w)?;
        cx.ret(signal_aborted(w, s))
    });
    getter(reg, rt, "AbortSignal", "reason", |cx, w| {
        let s = this_signal(cx, w)?;
        cx.ret(signal_reason(w, s).map_or(Arg::Undefined, Arg::Handle))
    });
    method(reg, rt, "AbortSignal", "throwIfAborted", |cx, w| {
        let s = this_signal(cx, w)?;
        if signal_aborted(w, s) {
            let r = signal_reason(w, s);
            return Err(HostError::Throw(r.map_or(Arg::Undefined, Arg::Handle)));
        }
        Ok(())
    });
    reg.register(
        "AbortSignal",
        "abort",
        ImplKind::StaticMethod,
        super::wf(rt, |cx, w| {
            let s = new_signal(w);
            let reason = if !cx.is_empty() && cx.arg_kind(0) != ValueKind::Undefined {
                Some(cx.arg_handle(0))
            } else {
                cx.make_handle(dom_exception(
                    w,
                    "AbortError",
                    "signal is aborted without reason",
                ))
                .ok()
            };
            w.with_obj(s, |o| {
                if let Obj::AbortSignal(d) = o {
                    d.aborted = true;
                    d.reason = reason;
                }
            });
            cx.ret(w.obj_arg(s))
        }),
    );
    reg.register(
        "AbortSignal",
        "timeout",
        ImplKind::StaticMethod,
        super::wf(rt, |cx, w| {
            need_args(cx, 1, "timeout", "AbortSignal")?;
            let ms = cx.arg_f64(0)?;
            let s = new_signal(w);
            w.add_timer(
                TimerCallback::Task(Some(Box::new(move |js, w| {
                    let reason = js.make(dom_exception(w, "TimeoutError", "signal timed out"));
                    abort_signal(js, w, s, reason);
                }))),
                ms,
                Vec::new(),
                false,
            );
            cx.ret(w.obj_arg(s))
        }),
    );
    reg.register(
        "AbortSignal",
        "any",
        ImplKind::StaticMethod,
        super::wf(rt, |cx, w| {
            need_args(cx, 1, "any", "AbortSignal")?;
            let s = new_signal(w);
            let list = cx.arg_handle(0);
            let len = cx.get_property(list, "length")?;
            let n = match cx.handle_value(len) {
                lb_js::JsValue::Number(n) => n as u32,
                _ => 0,
            };
            cx.release(len);
            for i in 0..n {
                let item = cx.get_property(list, &i.to_string())?;
                if let Some(src) = cx
                    .handle_host(item)
                    .filter(|r| r.kind == K_OBJ)
                    .map(|r| r.id)
                {
                    if signal_aborted(w, src) {
                        let reason = signal_reason(w, src);
                        w.with_obj(s, |o| {
                            if let Obj::AbortSignal(d) = o
                                && !d.aborted
                            {
                                d.aborted = true;
                                d.reason = reason;
                            }
                        });
                    } else {
                        signal_on_abort(w, src, AbortAlgo::Dependent(s));
                    }
                }
                cx.release(item);
            }
            cx.release(list);
            cx.ret(w.obj_arg(s))
        }),
    );

    // Headers
    ctor(reg, rt, "Headers", |cx, w| {
        let mut h = HeadersData::default();
        fill_headers(cx, w, 0, &mut h)?;
        let id = new_headers(w, h);
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    fn hd(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<u32, HostError> {
        let id = this_obj(cx)?;
        match w.with_obj(id, |o| matches!(o, Obj::Headers(_))) {
            Some(true) => Ok(id),
            _ => Err(illegal()),
        }
    }
    fn hmut(
        w: &Window,
        id: u32,
        op: &str,
        f: impl FnOnce(&mut HeadersData),
    ) -> Result<(), HostError> {
        w.with_obj(id, |o| match o {
            Obj::Headers(h) if h.immutable => Err(HostError::type_error(format!(
                "Failed to execute '{op}' on 'Headers': Headers are immutable"
            ))),
            Obj::Headers(h) => {
                f(h);
                Ok(())
            }
            _ => Err(illegal()),
        })
        .unwrap_or_else(|| Err(illegal()))
    }
    for (name, nargs) in [("append", 2), ("set", 2)] {
        method(reg, rt, "Headers", name, move |cx, w| {
            need_args(cx, nargs, name, "Headers")?;
            let id = hd(cx, w)?;
            let (k, v) = (cx.arg_string(0)?, cx.arg_string(1)?);
            if !valid_name(&k) {
                return Err(HostError::type_error(format!(
                    "Failed to execute '{name}' on 'Headers': Invalid name"
                )));
            }
            hmut(w, id, name, |h| {
                if name == "append" {
                    h.append(&k, &v)
                } else {
                    h.set(&k, &v)
                }
            })
        });
    }
    method(reg, rt, "Headers", "delete", |cx, w| {
        need_args(cx, 1, "delete", "Headers")?;
        let id = hd(cx, w)?;
        let k = cx.arg_string(0)?.to_ascii_lowercase();
        hmut(w, id, "delete", |h| h.list.retain(|(n, _)| *n != k))
    });
    method(reg, rt, "Headers", "get", |cx, w| {
        need_args(cx, 1, "get", "Headers")?;
        let id = hd(cx, w)?;
        let k = cx.arg_string(0)?;
        match headers_of(w, id).and_then(|h| h.get(&k)) {
            Some(v) => cx.ret(v),
            None => cx.ret(Arg::Null),
        }
    });
    method(reg, rt, "Headers", "getSetCookie", |cx, w| {
        let id = hd(cx, w)?;
        let v: Vec<Arg> = headers_of(w, id)
            .map(|h| {
                h.list
                    .into_iter()
                    .filter(|(k, _)| k == "set-cookie")
                    .map(|(_, v)| Arg::String(v))
                    .collect()
            })
            .unwrap_or_default();
        cx.ret(Arg::Array(v))
    });
    method(reg, rt, "Headers", "has", |cx, w| {
        need_args(cx, 1, "has", "Headers")?;
        let id = hd(cx, w)?;
        let k = cx.arg_string(0)?;
        cx.ret(headers_of(w, id).and_then(|h| h.get(&k)).is_some())
    });
    for (name, part) in [("entries", 0), ("keys", 1), ("values", 2)] {
        method(reg, rt, "Headers", name, move |cx, w| {
            let id = hd(cx, w)?;
            let items: Vec<Arg> = headers_of(w, id)
                .map(|h| h.sorted())
                .unwrap_or_default()
                .into_iter()
                .map(|(k, v)| match part {
                    0 => Arg::Array(vec![Arg::String(k), Arg::String(v)]),
                    1 => Arg::String(k),
                    _ => Arg::String(v),
                })
                .collect();
            super::misc::ret_iterator(cx, w, items)
        });
    }
    method(reg, rt, "Headers", "forEach", |cx, w| {
        need_args(cx, 1, "forEach", "Headers")?;
        let id = hd(cx, w)?;
        if cx.arg_kind(0) != ValueKind::Function {
            return Err(HostError::type_error(
                "Failed to execute 'forEach' on 'Headers': The callback provided as parameter 1 is not a function.",
            ));
        }
        let f = cx.arg_handle(0);
        let this = if cx.len() > 1 {
            Arg::Handle(cx.arg_handle(1))
        } else {
            Arg::Undefined
        };
        let me = w.obj_arg(id);
        for (k, v) in headers_of(w, id).map(|h| h.sorted()).unwrap_or_default() {
            cx.call(
                f,
                this.clone(),
                &[Arg::String(v), Arg::String(k), me.clone()],
            )?;
        }
        Ok(())
    });

    // fetch
    method(reg, rt, "Window", "fetch", do_fetch);

    // Request
    ctor(reg, rt, "Request", |cx, w| {
        let id = build_request(cx, w, "Request", "Request")?;
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    fn rq<R>(
        cx: &mut CallCtx<'_, '_, '_>,
        w: &Window,
        f: impl FnOnce(&RequestData) -> R,
    ) -> Result<R, HostError> {
        let id = this_obj(cx)?;
        w.with_obj(id, |o| match o {
            Obj::Request(r) => Some(f(r)),
            _ => None,
        })
        .flatten()
        .ok_or_else(illegal)
    }
    getter(reg, rt, "Request", "method", |cx, w| {
        let v = rq(cx, w, |r| r.method.clone())?;
        cx.ret(v)
    });
    getter(reg, rt, "Request", "url", |cx, w| {
        let v = rq(cx, w, |r| r.url.to_string())?;
        cx.ret(v)
    });
    getter(reg, rt, "Request", "headers", |cx, w| {
        let v = rq(cx, w, |r| r.headers)?;
        cx.ret(w.obj_arg(v))
    });
    getter(reg, rt, "Request", "signal", |cx, w| {
        let v = rq(cx, w, |r| r.signal)?;
        cx.ret(w.obj_arg(v))
    });
    getter(reg, rt, "Request", "mode", |cx, w| {
        let v = rq(cx, w, |r| r.mode)?;
        cx.ret(v)
    });
    getter(reg, rt, "Request", "credentials", |cx, w| {
        let v = rq(cx, w, |r| r.credentials.clone())?;
        cx.ret(v)
    });
    getter(reg, rt, "Request", "cache", |cx, w| {
        let v = rq(cx, w, |r| r.cache.clone())?;
        cx.ret(v)
    });
    getter(reg, rt, "Request", "redirect", |cx, w| {
        let v = rq(cx, w, |r| r.redirect.clone())?;
        cx.ret(v)
    });
    getter(reg, rt, "Request", "destination", |cx, w| {
        rq(cx, w, |_| ())?;
        cx.ret("")
    });
    getter(reg, rt, "Request", "referrer", |cx, w| {
        rq(cx, w, |_| ())?;
        cx.ret("about:client")
    });
    getter(reg, rt, "Request", "referrerPolicy", |cx, w| {
        rq(cx, w, |_| ())?;
        cx.ret("")
    });
    getter(reg, rt, "Request", "integrity", |cx, w| {
        rq(cx, w, |_| ())?;
        cx.ret("")
    });
    getter(reg, rt, "Request", "keepalive", |cx, w| {
        rq(cx, w, |_| ())?;
        cx.ret(false)
    });
    getter(reg, rt, "Request", "bodyUsed", |cx, w| {
        let v = rq(cx, w, |r| r.used)?;
        cx.ret(v)
    });
    method(reg, rt, "Request", "clone", |cx, w| {
        let (m, u, h, b, used, mode, cred, cache, redirect, sig) = rq(cx, w, |r| {
            (
                r.method.clone(),
                r.url.clone(),
                r.headers,
                r.body.clone(),
                r.used,
                r.mode,
                r.credentials.clone(),
                r.cache.clone(),
                r.redirect.clone(),
                r.signal,
            )
        })?;
        if used {
            return Err(HostError::type_error(
                "Failed to execute 'clone' on 'Request': Request body is already used",
            ));
        }
        let h2 = new_headers(w, headers_of(w, h).unwrap_or_default());
        let s2 = new_signal(w);
        signal_on_abort(w, sig, AbortAlgo::Dependent(s2));
        let id = w.new_obj(Obj::Request(Box::new(RequestData {
            method: m,
            url: u,
            headers: h2,
            body: b,
            used: false,
            signal: s2,
            mode,
            credentials: cred,
            cache,
            redirect,
        })));
        cx.ret(w.obj_arg(id))
    });
    for name in ["text", "json", "arrayBuffer", "blob", "bytes"] {
        body_method(reg, rt, "Request", name);
        body_method(reg, rt, "Response", name);
    }
    method(reg, rt, "Response", "formData", |cx, _w| {
        let r = cx.ret_new_promise()?;
        cx.reject_promise(
            r,
            HostError::type_error("Failed to execute 'formData' on 'Response': not supported"),
        )
    });

    // Response
    ctor(reg, rt, "Response", |cx, w| {
        let body = extract_body(cx, w, 0)?;
        let init = Dict::arg(cx, 1);
        let status = init.number(cx, "status")?.unwrap_or(200.0);
        if !(200.0..=599.0).contains(&status) {
            init.release(cx);
            return Err(HostError::range_error(format!(
                "Failed to construct 'Response': The status provided ({status}) is outside the range [200, 599]."
            )));
        }
        let status_text = init.string(cx, "statusText")?.unwrap_or_default();
        let mut h = HeadersData::default();
        if let Some(hh) = init.get(cx, "headers")? {
            if let Some(src) = cx
                .handle_host(hh)
                .filter(|r| r.kind == K_OBJ)
                .and_then(|r| headers_of(w, r.id))
            {
                h = src;
            } else if let lb_js::JsValue::Json(s) = cx.handle_value(hh)
                && let Ok(serde_json::Value::Object(m)) =
                    serde_json::from_str::<serde_json::Value>(&s)
            {
                for (k, v) in m {
                    h.append(&k, &json_str(&v));
                }
            }
            cx.release(hh);
        }
        init.release(cx);
        h.immutable = false;
        if let Some((_, Some(ct))) = &body
            && h.get("content-type").is_none()
        {
            h.append("content-type", ct);
        }
        let hid = new_headers(w, h);
        let id = w.new_obj(Obj::Response(Box::new(ResponseData {
            status: status as u16,
            status_text,
            headers: hid,
            body: body.map(|b| b.0),
            used: false,
            url: String::new(),
            redirected: false,
            type_: "default",
        })));
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    fn rs<R>(
        cx: &mut CallCtx<'_, '_, '_>,
        w: &Window,
        f: impl FnOnce(&ResponseData) -> R,
    ) -> Result<R, HostError> {
        let id = this_obj(cx)?;
        w.with_obj(id, |o| match o {
            Obj::Response(r) => Some(f(r)),
            _ => None,
        })
        .flatten()
        .ok_or_else(illegal)
    }
    getter(reg, rt, "Response", "status", |cx, w| {
        let v = rs(cx, w, |r| r.status)?;
        cx.ret(u32::from(v))
    });
    getter(reg, rt, "Response", "ok", |cx, w| {
        let v = rs(cx, w, |r| (200..300).contains(&r.status))?;
        cx.ret(v)
    });
    getter(reg, rt, "Response", "statusText", |cx, w| {
        let v = rs(cx, w, |r| r.status_text.clone())?;
        cx.ret(v)
    });
    getter(reg, rt, "Response", "headers", |cx, w| {
        let v = rs(cx, w, |r| r.headers)?;
        cx.ret(w.obj_arg(v))
    });
    getter(reg, rt, "Response", "url", |cx, w| {
        let v = rs(cx, w, |r| r.url.clone())?;
        cx.ret(v)
    });
    getter(reg, rt, "Response", "redirected", |cx, w| {
        let v = rs(cx, w, |r| r.redirected)?;
        cx.ret(v)
    });
    getter(reg, rt, "Response", "type", |cx, w| {
        let v = rs(cx, w, |r| r.type_)?;
        cx.ret(v)
    });
    getter(reg, rt, "Response", "bodyUsed", |cx, w| {
        let v = rs(cx, w, |r| r.used)?;
        cx.ret(v)
    });
    getter(reg, rt, "Response", "body", |cx, w| {
        rs(cx, w, |_| ())?;
        cx.ret(Arg::Null)
    });
    method(reg, rt, "Response", "clone", |cx, w| {
        let (status, st, h, body, used, url, red, ty) = rs(cx, w, |r| {
            (
                r.status,
                r.status_text.clone(),
                r.headers,
                r.body.clone(),
                r.used,
                r.url.clone(),
                r.redirected,
                r.type_,
            )
        })?;
        if used {
            return Err(HostError::type_error(
                "Failed to execute 'clone' on 'Response': Response body is already used",
            ));
        }
        let h2 = new_headers(w, headers_of(w, h).unwrap_or_default());
        let id = w.new_obj(Obj::Response(Box::new(ResponseData {
            status,
            status_text: st,
            headers: h2,
            body,
            used: false,
            url,
            redirected: red,
            type_: ty,
        })));
        cx.ret(w.obj_arg(id))
    });
    reg.register(
        "Response",
        "error",
        ImplKind::StaticMethod,
        super::wf(rt, |cx, w| {
            let h = new_headers(
                w,
                HeadersData {
                    list: Vec::new(),
                    immutable: true,
                },
            );
            let id = w.new_obj(Obj::Response(Box::new(ResponseData {
                status: 0,
                status_text: String::new(),
                headers: h,
                body: None,
                used: false,
                url: String::new(),
                redirected: false,
                type_: "error",
            })));
            cx.ret(w.obj_arg(id))
        }),
    );
    reg.register(
        "Response",
        "json",
        ImplKind::StaticMethod,
        super::wf(rt, |cx, w| {
            need_args(cx, 1, "json", "Response")?;
            let text = match cx.arg_value(0) {
                lb_js::JsValue::Json(s) => s,
                lb_js::JsValue::String(s) => super::js_string_literal(&s),
                lb_js::JsValue::Number(n) => n.to_string(),
                lb_js::JsValue::Bool(b) => b.to_string(),
                lb_js::JsValue::Null => "null".into(),
                _ => {
                    return Err(HostError::type_error(
                        "Failed to execute 'json' on 'Response': The data is not JSON serializable",
                    ));
                }
            };
            let mut h = HeadersData::default();
            h.append("content-type", "application/json");
            let hid = new_headers(w, h);
            let id = w.new_obj(Obj::Response(Box::new(ResponseData {
                status: 200,
                status_text: String::new(),
                headers: hid,
                body: Some(text.into_bytes()),
                used: false,
                url: String::new(),
                redirected: false,
                type_: "default",
            })));
            cx.ret(w.obj_arg(id))
        }),
    );
    reg.register(
        "Response",
        "redirect",
        ImplKind::StaticMethod,
        super::wf(rt, |cx, w| {
            need_args(cx, 1, "redirect", "Response")?;
            let s = cx.arg_string(0)?;
            let u = w.resolve_url(&s).ok_or_else(|| {
                HostError::type_error(format!(
                    "Failed to execute 'redirect' on 'Response': Failed to parse URL from {s}"
                ))
            })?;
            let status = if cx.len() > 1 {
                cx.arg_u32(1)? as u16
            } else {
                302
            };
            if ![301, 302, 303, 307, 308].contains(&status) {
                return Err(HostError::range_error(
                    "Failed to execute 'redirect' on 'Response': Invalid status code",
                ));
            }
            let mut h = HeadersData::default();
            h.append("location", u.as_str());
            h.immutable = true;
            let hid = new_headers(w, h);
            let id = w.new_obj(Obj::Response(Box::new(ResponseData {
                status,
                status_text: String::new(),
                headers: hid,
                body: None,
                used: false,
                url: String::new(),
                redirected: false,
                type_: "default",
            })));
            cx.ret(w.obj_arg(id))
        }),
    );

    // XMLHttpRequest
    ctor(reg, rt, "XMLHttpRequest", |cx, w| {
        let id = w.new_obj(Obj::Xhr(Box::new(XhrState {
            response_type: String::new(),
            is_async: true,
            ..Default::default()
        })));
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    method(reg, rt, "XMLHttpRequest", "open", |cx, w| {
        need_args(cx, 2, "open", "XMLHttpRequest")?;
        let id = this_xhr(cx, w)?;
        let m = cx.arg_string(0)?;
        let u = cx.arg_string(1)?;
        let is_async = cx.len() < 3 || cx.arg_bool(2);
        let up = m.to_ascii_uppercase();
        if matches!(up.as_str(), "CONNECT" | "TRACE" | "TRACK") {
            return Err(w.rt.err("SecurityError", format!("Failed to execute 'open' on 'XMLHttpRequest': '{m}' HTTP method is unsupported.")));
        }
        let url = w.resolve_url(&u).ok_or_else(|| {
            w.rt.err(
                "SyntaxError",
                "Failed to execute 'open' on 'XMLHttpRequest': Invalid URL",
            )
        })?;
        let method = if matches!(
            up.as_str(),
            "DELETE" | "GET" | "HEAD" | "OPTIONS" | "POST" | "PUT" | "PATCH"
        ) {
            up
        } else {
            m
        };
        let changed = with_xhr(w, id, |x| {
            x.generation += 1;
            x.method = method;
            x.url = Some(url);
            x.headers.clear();
            x.is_async = is_async;
            x.send_flag = false;
            x.status = 0;
            x.status_text.clear();
            x.resp_headers.clear();
            x.body.clear();
            x.error = false;
            x.json = None;
            let changed = x.ready_state != 1;
            x.ready_state = 1;
            changed
        })
        .unwrap_or(false);
        if changed {
            xhr_rsc(cx, w, id);
        }
        Ok(())
    });
    method(reg, rt, "XMLHttpRequest", "setRequestHeader", |cx, w| {
        need_args(cx, 2, "setRequestHeader", "XMLHttpRequest")?;
        let id = this_xhr(cx, w)?;
        let (k, v) = (cx.arg_string(0)?, cx.arg_string(1)?);
        let ok = with_xhr(w, id, |x| x.ready_state == 1 && !x.send_flag).unwrap_or(false);
        if !ok {
            return Err(w.rt.err("InvalidStateError", "Failed to execute 'setRequestHeader' on 'XMLHttpRequest': The object's state must be OPENED."));
        }
        if !valid_name(&k) {
            return Err(w.rt.err("SyntaxError", format!("Failed to execute 'setRequestHeader' on 'XMLHttpRequest': '{k}' is not a valid HTTP header field name.")));
        }
        let lk = k.to_ascii_lowercase();
        const FORBIDDEN: &[&str] = &[
            "accept-charset",
            "accept-encoding",
            "access-control-request-headers",
            "access-control-request-method",
            "connection",
            "content-length",
            "cookie",
            "cookie2",
            "date",
            "dnt",
            "expect",
            "host",
            "keep-alive",
            "origin",
            "referer",
            "te",
            "trailer",
            "transfer-encoding",
            "upgrade",
            "via",
        ];
        if FORBIDDEN.contains(&lk.as_str()) || lk.starts_with("proxy-") || lk.starts_with("sec-") {
            w.log(crate::host::ConsoleMessage {
                level: "error".into(),
                text: format!("Refused to set unsafe header \"{k}\""),
                url: None,
                line: None,
            });
            return Ok(());
        }
        with_xhr(w, id, |x| {
            match x
                .headers
                .iter_mut()
                .find(|(n, _)| n.eq_ignore_ascii_case(&k))
            {
                Some(e) => {
                    e.1.push_str(", ");
                    e.1.push_str(&normalize_value(&v));
                }
                None => x.headers.push((k, normalize_value(&v))),
            }
        });
        Ok(())
    });
    method(reg, rt, "XMLHttpRequest", "send", |cx, w| {
        let id = this_xhr(cx, w)?;
        let (state, sent, method) =
            with_xhr(w, id, |x| (x.ready_state, x.send_flag, x.method.clone()))
                .ok_or_else(illegal)?;
        if state != 1 || sent {
            return Err(w.rt.err(
                "InvalidStateError",
                "Failed to execute 'send' on 'XMLHttpRequest': The object's state must be OPENED.",
            ));
        }
        let body = if matches!(method.as_str(), "GET" | "HEAD") {
            None
        } else {
            extract_body(cx, w, 0)?
        };
        let (url, mut headers, is_async, generation, timeout) = with_xhr(w, id, |x| {
            x.send_flag = true;
            x.error = false;
            (
                x.url.clone(),
                x.headers.clone(),
                x.is_async,
                x.generation,
                x.timeout,
            )
        })
        .ok_or_else(illegal)?;
        let Some(url) = url else {
            return Err(illegal());
        };
        if !is_async {
            w.warn("Synchronous XMLHttpRequest is not supported; the request runs asynchronously.");
        }
        if let Some((_, Some(ct))) = &body
            && !headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
        {
            headers.push(("Content-Type".into(), ct.clone()));
        }
        let origin = w.url().origin();
        let req = NetRequest {
            method,
            url,
            headers,
            body: body.map(|b| b.0).unwrap_or_default(),
            destination: Destination::Empty,
            initiator: Some(w.url()),
            mode: "cors",
        };
        xhr_event(cx, w, id, "loadstart", 0.0, 0.0);
        if timeout > 0 {
            w.add_timer(
                TimerCallback::Task(Some(Box::new(move |js, w| {
                    let pending = with_xhr(w, id, |x| x.generation == generation && x.send_flag)
                        .unwrap_or(false);
                    if pending {
                        with_xhr(w, id, |x| {
                            x.generation += 1;
                            x.send_flag = false;
                            x.ready_state = 4;
                            x.error = true;
                        });
                        xhr_rsc(js, w, id);
                        xhr_event(js, w, id, "timeout", 0.0, 0.0);
                        xhr_event(js, w, id, "loadend", 0.0, 0.0);
                    }
                }))),
                f64::from(timeout),
                Vec::new(),
                false,
            );
        }
        start_request(
            w,
            req,
            Box::new(move |js, w, r| xhr_complete(js, w, id, generation, r, origin)),
        );
        Ok(())
    });
    method(reg, rt, "XMLHttpRequest", "abort", |cx, w| {
        let id = this_xhr(cx, w)?;
        let was = with_xhr(w, id, |x| {
            x.generation += 1;
            let was = x.send_flag || matches!(x.ready_state, 2 | 3);
            x.send_flag = false;
            x.body.clear();
            x.resp_headers.clear();
            x.status = 0;
            if was {
                x.ready_state = 4;
            }
            was
        })
        .unwrap_or(false);
        if was {
            xhr_rsc(cx, w, id);
            xhr_event(cx, w, id, "abort", 0.0, 0.0);
            xhr_event(cx, w, id, "loadend", 0.0, 0.0);
            with_xhr(w, id, |x| x.ready_state = 0);
        }
        Ok(())
    });
    getter(reg, rt, "XMLHttpRequest", "readyState", |cx, w| {
        let id = this_xhr(cx, w)?;
        cx.ret(u32::from(with_xhr(w, id, |x| x.ready_state).unwrap_or(0)))
    });
    getter(reg, rt, "XMLHttpRequest", "status", |cx, w| {
        let id = this_xhr(cx, w)?;
        cx.ret(u32::from(with_xhr(w, id, |x| x.status).unwrap_or(0)))
    });
    getter(reg, rt, "XMLHttpRequest", "statusText", |cx, w| {
        let id = this_xhr(cx, w)?;
        cx.ret(with_xhr(w, id, |x| x.status_text.clone()).unwrap_or_default())
    });
    getter(reg, rt, "XMLHttpRequest", "responseURL", |cx, w| {
        let id = this_xhr(cx, w)?;
        cx.ret(with_xhr(w, id, |x| x.response_url.clone()).unwrap_or_default())
    });
    getter(reg, rt, "XMLHttpRequest", "responseText", |cx, w| {
        let id = this_xhr(cx, w)?;
        let (rt_, state) =
            with_xhr(w, id, |x| (x.response_type.clone(), x.ready_state)).unwrap_or_default();
        if !rt_.is_empty() && rt_ != "text" {
            return Err(w.rt.err("InvalidStateError", format!("Failed to read the 'responseText' property from 'XMLHttpRequest': The value is only accessible if the object's 'responseType' is '' or 'text' (was '{rt_}').")));
        }
        if state < 3 {
            return cx.ret("");
        }
        cx.ret(with_xhr(w, id, |x| xhr_text(x)).unwrap_or_default())
    });
    getter(reg, rt, "XMLHttpRequest", "response", |cx, w| {
        let id = this_xhr(cx, w)?;
        let (ty, state, err) =
            with_xhr(w, id, |x| (x.response_type.clone(), x.ready_state, x.error))
                .unwrap_or_default();
        match ty.as_str() {
            "" | "text" => {
                if state < 3 {
                    return cx.ret("");
                }
                cx.ret(with_xhr(w, id, |x| xhr_text(x)).unwrap_or_default())
            }
            _ if state != 4 || err => cx.ret(Arg::Null),
            "json" => {
                if let Some(Some(h)) = with_xhr(w, id, |x| x.json) {
                    return cx.ret(Arg::Handle(h));
                }
                let text = with_xhr(w, id, |x| xhr_text(x)).unwrap_or_default();
                if serde_json::from_str::<serde_json::Value>(&text).is_err() {
                    return cx.ret(Arg::Null);
                }
                let h = cx.make_handle(Arg::Json(text))?;
                with_xhr(w, id, |x| x.json = Some(h));
                cx.ret(Arg::Handle(h))
            }
            "arraybuffer" => cx.ret(Arg::ArrayBuffer(
                with_xhr(w, id, |x| x.body.clone()).unwrap_or_default(),
            )),
            "blob" => {
                let (bytes, ct) = with_xhr(w, id, |x| {
                    (
                        x.body.clone(),
                        x.resp_headers
                            .iter()
                            .find(|(k, _)| k == "content-type")
                            .map(|(_, v)| v.clone()),
                    )
                })
                .unwrap_or_default();
                let b = w.new_obj(Obj::Blob(Box::new(super::misc::BlobData {
                    bytes,
                    type_: ct.unwrap_or_default(),
                })));
                cx.ret(w.obj_arg(b))
            }
            "document" => {
                let text = with_xhr(w, id, |x| xhr_text(x)).unwrap_or_default();
                let slot = w.add_doc(lb_dom::Document::parse_html(&text));
                cx.ret(w.node_arg(slot, lb_dom::Document::DOCUMENT))
            }
            _ => cx.ret(Arg::Null),
        }
    });
    getter(reg, rt, "XMLHttpRequest", "responseXML", |cx, w| {
        this_xhr(cx, w)?;
        cx.ret(Arg::Null)
    });
    getter(reg, rt, "XMLHttpRequest", "responseType", |cx, w| {
        let id = this_xhr(cx, w)?;
        cx.ret(with_xhr(w, id, |x| x.response_type.clone()).unwrap_or_default())
    });
    setter(reg, rt, "XMLHttpRequest", "responseType", |cx, w| {
        let id = this_xhr(cx, w)?;
        let v = cx.arg_string(0)?;
        if !matches!(
            v.as_str(),
            "" | "text" | "json" | "arraybuffer" | "blob" | "document"
        ) {
            return Ok(());
        }
        if with_xhr(w, id, |x| x.ready_state >= 3).unwrap_or(false) {
            return Err(w.rt.err("InvalidStateError", "Failed to set the 'responseType' property on 'XMLHttpRequest': The response type cannot be set if the object's state is LOADING or DONE."));
        }
        with_xhr(w, id, |x| x.response_type = v);
        Ok(())
    });
    getter(reg, rt, "XMLHttpRequest", "timeout", |cx, w| {
        let id = this_xhr(cx, w)?;
        cx.ret(with_xhr(w, id, |x| x.timeout).unwrap_or(0))
    });
    setter(reg, rt, "XMLHttpRequest", "timeout", |cx, w| {
        let id = this_xhr(cx, w)?;
        let v = cx.arg_u32(0)?;
        with_xhr(w, id, |x| x.timeout = v);
        Ok(())
    });
    getter(reg, rt, "XMLHttpRequest", "withCredentials", |cx, w| {
        let id = this_xhr(cx, w)?;
        cx.ret(with_xhr(w, id, |x| x.with_credentials).unwrap_or(false))
    });
    setter(reg, rt, "XMLHttpRequest", "withCredentials", |cx, w| {
        let id = this_xhr(cx, w)?;
        let v = cx.arg_bool(0);
        with_xhr(w, id, |x| x.with_credentials = v);
        Ok(())
    });
    getter(reg, rt, "XMLHttpRequest", "upload", |cx, w| {
        let id = this_xhr(cx, w)?;
        let up = match with_xhr(w, id, |x| x.upload).flatten() {
            Some(u) => u,
            None => {
                let u = w.new_obj(Obj::XhrUpload);
                with_xhr(w, id, |x| x.upload = Some(u));
                u
            }
        };
        cx.ret(w.obj_arg(up))
    });
    method(reg, rt, "XMLHttpRequest", "getResponseHeader", |cx, w| {
        need_args(cx, 1, "getResponseHeader", "XMLHttpRequest")?;
        let id = this_xhr(cx, w)?;
        let k = cx.arg_string(0)?.to_ascii_lowercase();
        let v = with_xhr(w, id, |x| {
            let vals: Vec<&str> = x
                .resp_headers
                .iter()
                .filter(|(n, _)| *n == k)
                .map(|(_, v)| v.as_str())
                .collect();
            (!vals.is_empty()).then(|| vals.join(", "))
        })
        .flatten();
        cx.ret(v.map_or(Arg::Null, Arg::String))
    });
    method(
        reg,
        rt,
        "XMLHttpRequest",
        "getAllResponseHeaders",
        |cx, w| {
            let id = this_xhr(cx, w)?;
            let v = with_xhr(w, id, |x| {
                let mut h = HeadersData::default();
                for (k, v) in &x.resp_headers {
                    h.append(k, v);
                }
                h.sorted()
                    .into_iter()
                    .map(|(k, v)| format!("{k}: {v}\r\n"))
                    .collect::<String>()
            })
            .unwrap_or_default();
            cx.ret(v)
        },
    );
    method(reg, rt, "XMLHttpRequest", "overrideMimeType", |cx, w| {
        let id = this_xhr(cx, w)?;
        let v = cx.arg_string(0)?;
        with_xhr(w, id, |x| x.mime_override = Some(v));
        Ok(())
    });
    let _ = opt_string;
}

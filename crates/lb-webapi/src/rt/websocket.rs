//! `WebSocket` as in Chrome 153 (ADR 0025): constructor checks and texts,
//! `readyState`, `bufferedAmount` (grows on `send`, shrinks once the bytes
//! are on the wire), `binaryType`, `protocol`, `extensions`, `close()`
//! checks, and the `open` / `message` / `error` / `close` events in
//! Chrome's order. The connection itself is the page layer's
//! ([`crate::host::NetHost::websocket`], lb-net's client).

use std::rc::Rc;

use lb_js::{Arg, CallCtx, HostError, HostRef, ValueKind};
use tokio::sync::mpsc::UnboundedSender;
use url::Url;

use super::events::{EventExtra, fire_event, fire_event_with};
use super::misc::BlobData;
use super::{
    Js, K_OBJ, Obj, Runtime, TaskSource, Window, ctor, getter, illegal, method, need_args, setter,
    this_obj,
};
use crate::host::{ConsoleMessage, WsCommand, WsEvent, WsOpen};
use crate::registry::Registry;

const CONNECTING: u8 = 0;
const OPEN: u8 = 1;
const CLOSING: u8 = 2;
const CLOSED: u8 = 3;

/// One `WebSocket` object.
pub(crate) struct WsState {
    url: Url,
    ready: u8,
    buffered: u64,
    arraybuffer: bool,
    protocol: String,
    extensions: String,
    commands: Option<UnboundedSender<WsCommand>>,
    /// The connection failed: the `close` event is still due.
    failed: bool,
}

fn with_ws<R>(w: &Window, id: u32, f: impl FnOnce(&mut WsState) -> R) -> Option<R> {
    w.with_obj(id, |o| match o {
        Obj::WebSocket(s) => Some(f(s)),
        _ => None,
    })
    .flatten()
}

fn this_ws(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<u32, HostError> {
    let id = this_obj(cx)?;
    with_ws(w, id, |_| ()).ok_or_else(illegal)?;
    Ok(id)
}

/// RFC 2616 token characters (subprotocol names).
fn is_token(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b > 0x20 && b < 0x7f && !b"()<>@,;:\\\"/[]?={}".contains(&b))
}

fn err(w: &Window, name: &str, op: &str, msg: &str) -> HostError {
    let m = if op == "construct" {
        format!("Failed to construct 'WebSocket': {msg}")
    } else {
        format!("Failed to execute '{op}' on 'WebSocket': {msg}")
    };
    if name == "TypeError" {
        HostError::TypeError(m)
    } else {
        w.rt.err(name, m)
    }
}

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    ctor(reg, rt, "WebSocket", |cx, w| {
        need_args(cx, 1, "construct", "WebSocket").map_err(|_| {
            HostError::type_error(
                "Failed to construct 'WebSocket': 1 argument required, but only 0 present.",
            )
        })?;
        let s = cx.arg_string(0)?;
        let Some(mut url) = w.resolve_url(&s) else {
            return Err(err(
                w,
                "SyntaxError",
                "construct",
                &format!("The URL '{s}' is invalid."),
            ));
        };
        match url.scheme() {
            "ws" | "wss" => {}
            "http" => {
                let _ = url.set_scheme("ws");
            }
            "https" => {
                let _ = url.set_scheme("wss");
            }
            other => {
                return Err(err(
                    w,
                    "SyntaxError",
                    "construct",
                    &format!(
                        "The URL's scheme must be either 'http', 'https', 'ws', or 'wss'. '{other}' is not allowed."
                    ),
                ));
            }
        }
        if let Some(f) = url.fragment() {
            return Err(err(
                w,
                "SyntaxError",
                "construct",
                &format!(
                    "The URL contains a fragment identifier ('{f}'). Fragment identifiers are not allowed in WebSocket URLs."
                ),
            ));
        }
        let mut protocols: Vec<String> = Vec::new();
        if cx.len() > 1 && cx.arg_kind(1) != ValueKind::Undefined {
            if cx.arg_kind(1) == ValueKind::Object {
                let h = cx.arg_handle(1);
                let l = cx.get_property(h, "length")?;
                let n = match cx.handle_value(l) {
                    lb_js::JsValue::Number(n) => n as usize,
                    _ => 0,
                };
                cx.release(l);
                for i in 0..n.min(1024) {
                    let item = cx.get_property(h, &i.to_string())?;
                    protocols.push(cx.handle_string(item)?);
                    cx.release(item);
                }
                cx.release(h);
            } else {
                protocols.push(cx.arg_string(1)?);
            }
        }
        for (i, p) in protocols.iter().enumerate() {
            if !is_token(p) {
                return Err(err(
                    w,
                    "SyntaxError",
                    "construct",
                    &format!("The subprotocol '{p}' is invalid."),
                ));
            }
            if protocols[..i].contains(p) {
                return Err(err(
                    w,
                    "SyntaxError",
                    "construct",
                    &format!("The subprotocol '{p}' is duplicated."),
                ));
            }
        }
        let page = w.url();
        if page.scheme() == "https" && url.scheme() == "ws" && !super::is_secure_url(&url) {
            return Err(err(
                w,
                "SecurityError",
                "construct",
                "An insecure WebSocket connection may not be initiated from a page loaded over HTTPS.",
            ));
        }
        let id = w.new_obj(Obj::WebSocket(Box::new(WsState {
            url: url.clone(),
            ready: CONNECTING,
            buffered: 0,
            arraybuffer: false,
            protocol: String::new(),
            extensions: String::new(),
            commands: None,
            failed: false,
        })));
        cx.init_this(HostRef::new(K_OBJ, id))?;
        let origin = super::frames::origin(w);
        match w.net.websocket(WsOpen {
            url,
            protocols,
            origin,
        }) {
            Some(ch) => {
                with_ws(w, id, |s| s.commands = Some(ch.commands));
                let mut events = ch.events;
                let weak = Rc::downgrade(w);
                w.net.spawn(Box::pin(async move {
                    while let Some(ev) = events.recv().await {
                        let Some(w) = weak.upgrade() else { break };
                        if w.is_closed() {
                            break;
                        }
                        w.queue_task(
                            TaskSource::Networking,
                            Box::new(move |js, w| on_event(js, w, id, ev)),
                        );
                    }
                }));
            }
            None => {
                w.queue_task(
                    TaskSource::Networking,
                    Box::new(move |js, w| {
                        on_event(js, w, id, WsEvent::Failed(String::new()));
                        on_event(
                            js,
                            w,
                            id,
                            WsEvent::Closed {
                                code: 1006,
                                reason: String::new(),
                                clean: false,
                            },
                        );
                    }),
                );
            }
        }
        Ok(())
    });
    getter(reg, rt, "WebSocket", "url", |cx, w| {
        let id = this_ws(cx, w)?;
        cx.ret(with_ws(w, id, |s| s.url.to_string()).unwrap_or_default())
    });
    getter(reg, rt, "WebSocket", "readyState", |cx, w| {
        let id = this_ws(cx, w)?;
        cx.ret(u32::from(with_ws(w, id, |s| s.ready).unwrap_or(CLOSED)))
    });
    getter(reg, rt, "WebSocket", "bufferedAmount", |cx, w| {
        let id = this_ws(cx, w)?;
        cx.ret(with_ws(w, id, |s| s.buffered as f64).unwrap_or(0.0))
    });
    getter(reg, rt, "WebSocket", "protocol", |cx, w| {
        let id = this_ws(cx, w)?;
        cx.ret(with_ws(w, id, |s| s.protocol.clone()).unwrap_or_default())
    });
    getter(reg, rt, "WebSocket", "extensions", |cx, w| {
        let id = this_ws(cx, w)?;
        cx.ret(with_ws(w, id, |s| s.extensions.clone()).unwrap_or_default())
    });
    getter(reg, rt, "WebSocket", "binaryType", |cx, w| {
        let id = this_ws(cx, w)?;
        let ab = with_ws(w, id, |s| s.arraybuffer).unwrap_or(false);
        cx.ret(if ab { "arraybuffer" } else { "blob" })
    });
    setter(reg, rt, "WebSocket", "binaryType", |cx, w| {
        let id = this_ws(cx, w)?;
        let v = cx.arg_string(0)?;
        match v.as_str() {
            "blob" => with_ws(w, id, |s| s.arraybuffer = false),
            "arraybuffer" => with_ws(w, id, |s| s.arraybuffer = true),
            // An invalid enum value is ignored (with a console warning).
            _ => {
                w.warn(format!(
                    "The provided value '{v}' is not a valid enum value of type BinaryType."
                ));
                None
            }
        };
        Ok(())
    });
    method(reg, rt, "WebSocket", "send", |cx, w| {
        let id = this_ws(cx, w)?;
        need_args(cx, 1, "send", "WebSocket")?;
        let state = with_ws(w, id, |s| s.ready).unwrap_or(CLOSED);
        if state == CONNECTING {
            return Err(err(
                w,
                "InvalidStateError",
                "send",
                "Still in CONNECTING state.",
            ));
        }
        let cmd = if let Some(bytes) = cx.arg_bytes(0) {
            WsCommand::Binary(bytes)
        } else if let Some(blob) = cx
            .arg_host(0)
            .filter(|h| h.kind == K_OBJ)
            .and_then(|h| {
                w.with_obj(h.id, |o| match o {
                    Obj::Blob(b) => Some(b.bytes.clone()),
                    _ => None,
                })
            })
            .flatten()
        {
            WsCommand::Binary(blob)
        } else {
            WsCommand::Text(cx.arg_string(0)?)
        };
        let len = match &cmd {
            WsCommand::Text(s) => s.len(),
            WsCommand::Binary(b) => b.len(),
            WsCommand::Close(..) => 0,
        } as u64;
        let sender = with_ws(w, id, |s| {
            s.buffered += len;
            s.commands.clone()
        })
        .flatten();
        if state != OPEN {
            w.log(ConsoleMessage {
                level: "error".into(),
                text: "WebSocket is already in CLOSING or CLOSED state.".into(),
                url: None,
                line: None,
            });
            return Ok(());
        }
        if let Some(tx) = sender {
            let _ = tx.send(cmd);
        }
        Ok(())
    });
    method(reg, rt, "WebSocket", "close", |cx, w| {
        let id = this_ws(cx, w)?;
        let code = if !cx.is_empty() && cx.arg_kind(0) != ValueKind::Undefined {
            // `[Clamp] unsigned short`.
            let n = cx.arg_f64(0)?;
            let c = if n.is_nan() {
                0.0
            } else {
                n.round().clamp(0.0, 65535.0)
            };
            let c = c as u16;
            if c != 1000 && !(3000..=4999).contains(&c) {
                return Err(err(
                    w,
                    "InvalidAccessError",
                    "close",
                    &format!(
                        "The close code must be either 1000, or between 3000 and 4999. {c} is neither."
                    ),
                ));
            }
            Some(c)
        } else {
            None
        };
        let reason = if cx.len() > 1 && cx.arg_kind(1) != ValueKind::Undefined {
            cx.arg_string(1)?
        } else {
            String::new()
        };
        if reason.len() > 123 {
            return Err(err(
                w,
                "SyntaxError",
                "close",
                "The close reason must not be greater than 123 UTF-8 bytes.",
            ));
        }
        let (state, tx) =
            with_ws(w, id, |s| (s.ready, s.commands.clone())).unwrap_or((CLOSED, None));
        match state {
            CONNECTING => {
                w.log(ConsoleMessage {
                    level: "error".into(),
                    text: format!(
                        "WebSocket connection to '{}' failed: WebSocket is closed before the connection is established.",
                        with_ws(w, id, |s| s.url.to_string()).unwrap_or_default()
                    ),
                    url: None,
                    line: None,
                });
                with_ws(w, id, |s| {
                    s.ready = CLOSING;
                    s.commands = None;
                });
                w.queue_task(
                    TaskSource::Networking,
                    Box::new(move |js, w| {
                        fire_error_and_close(js, w, id, 1006, String::new(), false, true);
                    }),
                );
            }
            OPEN => {
                with_ws(w, id, |s| s.ready = CLOSING);
                if let Some(tx) = tx {
                    let _ = tx.send(WsCommand::Close(code, reason));
                }
            }
            _ => {}
        }
        Ok(())
    });
}

fn fire_error_and_close(
    js: &mut dyn Js,
    w: &Rc<Window>,
    id: u32,
    code: u16,
    reason: String,
    clean: bool,
    error: bool,
) {
    let target = HostRef::new(K_OBJ, id);
    with_ws(w, id, |s| {
        s.ready = CLOSED;
        s.commands = None;
        s.failed = false;
    });
    if error {
        fire_event(js, w, target, "error", false, false);
    }
    fire_event_with(
        js,
        w,
        target,
        "CloseEvent",
        "close",
        false,
        false,
        EventExtra::Close {
            code,
            reason,
            was_clean: clean,
        },
    );
}

/// A connection event, in a networking task.
fn on_event(js: &mut dyn Js, w: &Rc<Window>, id: u32, ev: WsEvent) {
    let target = HostRef::new(K_OBJ, id);
    let Some((state, failed)) = with_ws(w, id, |s| (s.ready, s.failed)) else {
        return;
    };
    if state == CLOSED && !(failed && matches!(ev, WsEvent::Closed { .. })) {
        return;
    }
    match ev {
        WsEvent::Open {
            protocol,
            extensions,
        } => {
            if state != CONNECTING {
                return;
            }
            with_ws(w, id, |s| {
                s.ready = OPEN;
                s.protocol = protocol;
                s.extensions = extensions;
            });
            fire_event(js, w, target, "open", false, false);
        }
        WsEvent::Text(_) | WsEvent::Binary(_) => {
            if state != OPEN {
                return;
            }
            let (origin, ab) = with_ws(w, id, |s| {
                (s.url.origin().ascii_serialization(), s.arraybuffer)
            })
            .unwrap_or_default();
            let data = match ev {
                WsEvent::Text(t) => js.make(Arg::String(t)),
                WsEvent::Binary(b) if ab => js.make(Arg::ArrayBuffer(b)),
                WsEvent::Binary(b) => {
                    let blob = w.new_obj(Obj::Blob(Box::new(BlobData {
                        bytes: b,
                        type_: String::new(),
                    })));
                    js.make(w.obj_arg(blob))
                }
                _ => None,
            };
            fire_event_with(
                js,
                w,
                target,
                "MessageEvent",
                "message",
                false,
                false,
                EventExtra::Message {
                    data,
                    origin,
                    source: None,
                    ports: None,
                },
            );
        }
        WsEvent::Flushed(n) => {
            with_ws(w, id, |s| s.buffered = s.buffered.saturating_sub(n as u64));
        }
        WsEvent::Failed(detail) => {
            let url = with_ws(w, id, |s| s.url.to_string()).unwrap_or_default();
            w.log(ConsoleMessage {
                level: "error".into(),
                text: format!("WebSocket connection to '{url}' failed: {detail}"),
                url: None,
                line: None,
            });
            // Chrome: `readyState` is CLOSED when `error` fires.
            with_ws(w, id, |s| {
                s.ready = CLOSED;
                s.failed = true;
                s.commands = None;
            });
            fire_event(js, w, target, "error", false, false);
        }
        WsEvent::Closed {
            code,
            reason,
            clean,
        } => fire_error_and_close(js, w, id, code, reason, clean, false),
    }
}

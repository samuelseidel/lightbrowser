//! Minimal Chrome-shaped stubs for APIs Turnstile's soft-pass probes:
//! `navigator.locks`, `cookieStore`, and the Navigation API
//! (`window.navigation`). `navigator.storage` is the real manager in
//! `navigator.rs`. Unregistered IDL getters returned bare `undefined`;
//! these return real host objects.

use std::rc::Rc;

use lb_js::{Arg, CallCtx, HostError, ValueKind};

use super::{Js, Obj, Runtime, TaskSource, Window, getter, illegal, method, need_args, this_obj};
use crate::registry::Registry;

/// Objects owned by this module.
#[derive(Debug)]
pub(crate) enum PlatformObj {
    LockManager,
    CookieStore,
    Navigation,
    NavigationHistoryEntry,
}

impl PlatformObj {
    pub(crate) fn iface_name(&self) -> &'static str {
        match self {
            PlatformObj::LockManager => "LockManager",
            PlatformObj::CookieStore => "CookieStore",
            PlatformObj::Navigation => "Navigation",
            PlatformObj::NavigationHistoryEntry => "NavigationHistoryEntry",
        }
    }
}

fn this_plat(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    want: fn(&PlatformObj) -> bool,
) -> Result<(), HostError> {
    let id = this_obj(cx)?;
    match w.with_obj(id, |o| match o {
        Obj::Platform(p) => want(p),
        _ => false,
    }) {
        Some(true) => Ok(()),
        _ => Err(illegal()),
    }
}

fn plat_arg(w: &Window, key: &'static str, o: PlatformObj) -> Arg {
    let id = w.cached_obj((0, 0, key, String::new()), || Obj::Platform(Box::new(o)));
    w.obj_arg(id)
}

fn resolve_later(w: &Rc<Window>, r: lb_js::Resolver, value: Arg) {
    w.queue_task(
        TaskSource::Networking,
        Box::new(move |js: &mut dyn Js, _w: &Rc<Window>| {
            js.resolve(r, value);
        }),
    );
}

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    // `navigator.storage` is the real StorageManager in `navigator.rs`
    // (Chrome's reported quota and per-system usage). Registering a stub
    // here replaced it, because a later registration wins.
    getter(reg, rt, "Navigator", "locks", |cx, w| {
        this_obj(cx)?;
        cx.ret(plat_arg(w, "nav:locks", PlatformObj::LockManager))
    });
    getter(reg, rt, "Window", "cookieStore", |cx, w| {
        cx.ret(plat_arg(w, "win:cookieStore", PlatformObj::CookieStore))
    });
    getter(reg, rt, "Window", "navigation", |cx, w| {
        cx.ret(plat_arg(w, "win:navigation", PlatformObj::Navigation))
    });

    method(reg, rt, "LockManager", "request", |cx, w| {
        this_plat(cx, w, |p| matches!(p, PlatformObj::LockManager))?;
        need_args(cx, 2, "request", "LockManager")?;
        let _name = cx.arg_string(0)?;
        let cb_i = if cx.arg_kind(1) == ValueKind::Function {
            1
        } else if cx.len() >= 3 && cx.arg_kind(2) == ValueKind::Function {
            2
        } else {
            return Err(HostError::type_error(
                "Failed to execute 'request' on 'LockManager': Failed to convert value to 'Function'.",
            ));
        };
        let cb = cx.arg_handle(cb_i);
        let r = cx.ret_new_promise()?;
        let w = Rc::clone(w);
        w.queue_task(
            TaskSource::Networking,
            Box::new(move |js: &mut dyn Js, _w: &Rc<Window>| {
                match js.call_handle(cb, Arg::Undefined, &[Arg::Null]) {
                    Ok(h) => {
                        js.resolve(r, Arg::Handle(h));
                        js.release(h);
                    }
                    Err(e) => js.reject(r, HostError::Error(e.message)),
                }
                js.release(cb);
            }),
        );
        Ok(())
    });
    method(reg, rt, "LockManager", "query", |cx, w| {
        this_plat(cx, w, |p| matches!(p, PlatformObj::LockManager))?;
        let r = cx.ret_new_promise()?;
        resolve_later(w, r, Arg::Json(r#"{"held":[],"pending":[]}"#.into()));
        Ok(())
    });

    method(reg, rt, "CookieStore", "get", |cx, w| {
        this_plat(cx, w, |p| matches!(p, PlatformObj::CookieStore))?;
        let r = cx.ret_new_promise()?;
        resolve_later(w, r, Arg::Null);
        Ok(())
    });
    method(reg, rt, "CookieStore", "getAll", |cx, w| {
        this_plat(cx, w, |p| matches!(p, PlatformObj::CookieStore))?;
        let r = cx.ret_new_promise()?;
        resolve_later(w, r, Arg::Json("[]".into()));
        Ok(())
    });
    method(reg, rt, "CookieStore", "set", |cx, w| {
        this_plat(cx, w, |p| matches!(p, PlatformObj::CookieStore))?;
        let r = cx.ret_new_promise()?;
        resolve_later(w, r, Arg::Undefined);
        Ok(())
    });
    method(reg, rt, "CookieStore", "delete", |cx, w| {
        this_plat(cx, w, |p| matches!(p, PlatformObj::CookieStore))?;
        let r = cx.ret_new_promise()?;
        resolve_later(w, r, Arg::Undefined);
        Ok(())
    });

    getter(reg, rt, "Navigation", "currentEntry", |cx, w| {
        this_plat(cx, w, |p| matches!(p, PlatformObj::Navigation))?;
        cx.ret(plat_arg(
            w,
            "navapi:entry",
            PlatformObj::NavigationHistoryEntry,
        ))
    });
    getter(reg, rt, "Navigation", "transition", |cx, w| {
        this_plat(cx, w, |p| matches!(p, PlatformObj::Navigation))?;
        cx.ret(Arg::Null)
    });
    getter(reg, rt, "Navigation", "activation", |cx, w| {
        this_plat(cx, w, |p| matches!(p, PlatformObj::Navigation))?;
        cx.ret(Arg::Null)
    });
    getter(reg, rt, "Navigation", "canGoBack", |cx, w| {
        this_plat(cx, w, |p| matches!(p, PlatformObj::Navigation))?;
        cx.ret(false)
    });
    getter(reg, rt, "Navigation", "canGoForward", |cx, w| {
        this_plat(cx, w, |p| matches!(p, PlatformObj::Navigation))?;
        cx.ret(false)
    });
    method(reg, rt, "Navigation", "entries", |cx, w| {
        this_plat(cx, w, |p| matches!(p, PlatformObj::Navigation))?;
        let entry = plat_arg(w, "navapi:entry", PlatformObj::NavigationHistoryEntry);
        cx.ret(Arg::Array(vec![entry]))
    });

    getter(reg, rt, "NavigationHistoryEntry", "url", |cx, w| {
        this_plat(cx, w, |p| matches!(p, PlatformObj::NavigationHistoryEntry))?;
        cx.ret(w.url.borrow().as_str().to_owned())
    });
    getter(reg, rt, "NavigationHistoryEntry", "key", |cx, w| {
        this_plat(cx, w, |p| matches!(p, PlatformObj::NavigationHistoryEntry))?;
        cx.ret("lb-nav-key")
    });
    getter(reg, rt, "NavigationHistoryEntry", "id", |cx, w| {
        this_plat(cx, w, |p| matches!(p, PlatformObj::NavigationHistoryEntry))?;
        cx.ret("lb-nav-id")
    });
    getter(reg, rt, "NavigationHistoryEntry", "index", |cx, w| {
        this_plat(cx, w, |p| matches!(p, PlatformObj::NavigationHistoryEntry))?;
        cx.ret(0)
    });
    getter(
        reg,
        rt,
        "NavigationHistoryEntry",
        "sameDocument",
        |cx, w| {
            this_plat(cx, w, |p| matches!(p, PlatformObj::NavigationHistoryEntry))?;
            cx.ret(true)
        },
    );
    method(reg, rt, "NavigationHistoryEntry", "getState", |cx, w| {
        this_plat(cx, w, |p| matches!(p, PlatformObj::NavigationHistoryEntry))?;
        cx.ret(Arg::Undefined)
    });
}

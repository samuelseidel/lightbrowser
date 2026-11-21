//! Prioritized Task Scheduling (`window.scheduler`): enough for Turnstile's
//! challenge scripts (`postTask` / `yield`). Priority and abort signals are
//! ignored; delayed tasks use the page timer queue.

use std::rc::Rc;

use lb_js::{Arg, CallCtx, Handle, HostError, Resolver, ValueKind};

use super::{
    Dict, Js, Obj, Runtime, TaskSource, TimerCallback, Window, getter, illegal, method, need_args,
    this_obj,
};
use crate::registry::Registry;

fn this_scheduler(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<(), HostError> {
    let id = this_obj(cx)?;
    match w.with_obj(id, |o| matches!(o, Obj::Scheduler)) {
        Some(true) => Ok(()),
        _ => Err(illegal()),
    }
}

/// Register `scheduler` on Window / WorkerGlobalScope.
pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    for global in ["Window", "WorkerGlobalScope"] {
        getter(reg, rt, global, "scheduler", |cx, w| {
            let id = w.cached_obj((0, 0, "scheduler", String::new()), || Obj::Scheduler);
            cx.ret(w.obj_arg(id))
        });
    }

    method(reg, rt, "Scheduler", "postTask", post_task);
    method(reg, rt, "Scheduler", "yield", yield_task);
}

fn post_task(cx: &mut CallCtx<'_, '_, '_>, w: &Rc<Window>) -> Result<(), HostError> {
    this_scheduler(cx, w)?;
    need_args(cx, 1, "postTask", "Scheduler")?;
    if cx.arg_kind(0) != ValueKind::Function {
        return Err(HostError::type_error(
            "Failed to execute 'postTask' on 'Scheduler': The provided callback is not a function.",
        ));
    }
    let cb = cx.arg_handle(0);
    let opts = Dict::arg(cx, 1);
    let delay = opts.number(cx, "delay")?.unwrap_or(0.0).max(0.0);
    opts.release(cx);

    let r = cx.ret_new_promise()?;
    schedule(w, delay, r, Some(cb));
    Ok(())
}

fn yield_task(cx: &mut CallCtx<'_, '_, '_>, w: &Rc<Window>) -> Result<(), HostError> {
    this_scheduler(cx, w)?;
    let r = cx.ret_new_promise()?;
    schedule(w, 0.0, r, None);
    Ok(())
}

fn schedule(w: &Rc<Window>, delay: f64, r: Resolver, cb: Option<Handle>) {
    let run = Box::new(move |js: &mut dyn Js, _w: &Rc<Window>| match cb {
        None => js.resolve(r, Arg::Undefined),
        Some(cb) => {
            match js.call_handle(cb, Arg::Undefined, &[]) {
                Ok(h) => {
                    js.resolve(r, Arg::Handle(h));
                    js.release(h);
                }
                Err(e) => js.reject(r, HostError::Error(e.message)),
            }
            js.release(cb);
        }
    });
    if delay > 0.0 {
        w.add_timer(TimerCallback::Task(Some(run)), delay, Vec::new(), false);
    } else {
        w.queue_task(TaskSource::DomManipulation, run);
    }
}

//! `BroadcastChannel` as in Chrome 153, in windows and in dedicated,
//! shared and service worker scopes.
//!
//! Every open channel is listed in one process-wide table with its
//! browser context (the context's IndexedDB backend names it, see
//! [`ContextToken`]), its storage key (ADR 0037), its name and the
//! [`Inbox`] of the window or worker scope that owns it. `postMessage`
//! serializes the message once (V8 structured clone, as `postMessage` of
//! ports and workers) and sends a copy to the inbox of every other open
//! channel with the same context, storage key and name, in creation order; the receiving scope fires
//! `message` (or `messageerror`) as a posted-message task, on its own
//! thread, unless the channel was closed in between. So windows, frames,
//! workers and service workers of one context and storage partition reach
//! each other; a third-party frame's channels (and its workers') reach
//! only that partition, as in Chrome. Opaque origins reach no one.

use std::rc::Rc;
use std::sync::{Mutex, OnceLock};

use lb_js::{HostError, HostRef};

use super::idb::backend::ContextToken;
use super::worker::{Inbox, Payload, deliver, serialize_value};
use super::{K_OBJ, Obj, Runtime, Window, ctor, getter, illegal, method, need_args, this_obj};
use crate::registry::Registry;

/// One `BroadcastChannel` object.
pub(crate) struct BcState {
    name: String,
    closed: bool,
    /// Its row in [`table`].
    seq: u64,
}

struct Entry {
    seq: u64,
    context: Option<ContextToken>,
    /// Serialized storage key (it includes the origin).
    key: String,
    name: String,
    inbox: Inbox,
    obj: u32,
}

#[derive(Default)]
struct Table {
    next: u64,
    rows: Vec<Entry>,
}

/// The open channels of the process. Not fingerprint data: rows hold
/// channel names and inboxes, matched by context and storage key.
fn table() -> &'static Mutex<Table> {
    static T: OnceLock<Mutex<Table>> = OnceLock::new();
    T.get_or_init(Default::default)
}

fn lock() -> std::sync::MutexGuard<'static, Table> {
    table().lock().unwrap_or_else(|e| e.into_inner())
}

/// Drop the rows of a scope that is closing.
pub(crate) fn forget_inbox(inbox: &Inbox) {
    lock().rows.retain(|r| !r.inbox.same(inbox));
}

fn with_bc<R>(w: &Window, id: u32, f: impl FnOnce(&mut BcState) -> R) -> Option<R> {
    w.with_obj(id, |o| match o {
        Obj::Broadcast(s) => Some(f(s)),
        _ => None,
    })
    .flatten()
}

fn this_bc(cx: &mut lb_js::CallCtx<'_, '_, '_>, w: &Window) -> Result<u32, HostError> {
    let id = this_obj(cx)?;
    with_bc(w, id, |_| ()).ok_or_else(illegal)?;
    Ok(id)
}

fn same_context(a: &Option<ContextToken>, w: &Window) -> bool {
    match (a, w.idb_backend()) {
        (Some(t), Some(b)) => t.is(&b),
        (None, None) => true,
        _ => false,
    }
}

fn close(w: &Window, id: u32) {
    if let Some(seq) = with_bc(w, id, |s| {
        s.closed = true;
        s.seq
    }) {
        lock().rows.retain(|r| r.seq != seq);
    }
}

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    ctor(reg, rt, "BroadcastChannel", |cx, w| {
        if cx.is_empty() {
            return Err(HostError::type_error(
                "Failed to construct 'BroadcastChannel': 1 argument required, but only 0 present.",
            ));
        }
        let name = cx.arg_string(0)?;
        let key = w.storage_key_string();
        let inbox = w.inbox();
        let id = w.new_obj(Obj::Broadcast(Box::new(BcState {
            name: name.clone(),
            closed: false,
            seq: 0,
        })));
        let seq = {
            let mut t = lock();
            t.rows
                .retain(|r| !r.inbox.is_closed() && r.context.as_ref().is_none_or(|c| c.alive()));
            t.next += 1;
            let seq = t.next;
            t.rows.push(Entry {
                seq,
                context: w.idb_backend().map(|b| b.token()),
                key,
                name,
                inbox,
                obj: id,
            });
            seq
        };
        with_bc(w, id, |s| s.seq = seq);
        cx.init_this(HostRef::new(K_OBJ, id))?;
        Ok(())
    });
    getter(reg, rt, "BroadcastChannel", "name", |cx, w| {
        let id = this_bc(cx, w)?;
        cx.ret(with_bc(w, id, |s| s.name.clone()).unwrap_or_default())
    });
    method(reg, rt, "BroadcastChannel", "postMessage", |cx, w| {
        let id = this_bc(cx, w)?;
        need_args(cx, 1, "postMessage", "BroadcastChannel")?;
        let Some((name, seq, closed)) = with_bc(w, id, |s| (s.name.clone(), s.seq, s.closed))
        else {
            return Err(illegal());
        };
        if closed {
            return Err(w.rt.err(
                "InvalidStateError",
                "Failed to execute 'postMessage' on 'BroadcastChannel': Channel is closed",
            ));
        }
        let payload = serialize_value(cx, w, "postMessage", "BroadcastChannel")?;
        let origin = w.origin_string();
        if origin == "null" {
            return Ok(());
        }
        let key = w.storage_key_string();
        let targets: Vec<(Inbox, u32)> = lock()
            .rows
            .iter()
            .filter(|r| {
                r.seq != seq && r.name == name && r.key == key && same_context(&r.context, w)
            })
            .map(|r| (r.inbox.clone(), r.obj))
            .collect();
        for (inbox, obj) in targets {
            let p = payload.copy_without_ports();
            let origin = origin.clone();
            inbox.send(Box::new(move |js, w| on_message(js, w, obj, p, origin)));
        }
        Ok(())
    });
    method(reg, rt, "BroadcastChannel", "close", |cx, w| {
        let id = this_bc(cx, w)?;
        close(w, id);
        Ok(())
    });
}

fn on_message(js: &mut dyn super::Js, w: &Rc<Window>, obj: u32, p: Payload, origin: String) {
    if with_bc(w, obj, |s| s.closed) != Some(false) {
        return;
    }
    deliver(js, w, HostRef::new(K_OBJ, obj), p, origin, None);
}

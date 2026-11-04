//! IndexedDB as in Chrome 153 (ADR 0028): `IDBFactory`, connections,
//! transactions with Chrome's activation and auto-commit timing, object
//! stores, indexes, cursors, key ranges, `IDBRecord`, version change and
//! `blocked` events, Chrome's checks and error texts. Pages and workers.
//!
//! Data lives in the browser context's [`backend::IdbBackend`] (memory
//! only, per origin). This module keeps the per-window objects and turns
//! backend replies into events. A transaction is active while the task
//! that created it runs (it becomes inactive at the next task boundary,
//! [`task_boundary`]) and while one of its request events is dispatched;
//! it commits once it is inactive with no request left, as Chrome's
//! auto-commit does. Values are V8 structured clones; key paths are
//! evaluated on the clone; generated keys are injected when a value is
//! read (as Chrome does).

pub mod backend;
pub mod key;

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use lb_js::{Arg, CallCtx, Handle, HostError, HostRef, InterfaceId, JsValue, ValueKind};

use self::backend::{
    AllKind, DbMeta, Dir, Ev, IdbBackend, IndexMeta, Mode, Op, OpResult, Schema, Sink, Step,
    StoreMeta, Value,
};
use self::key::{Key, KeyPath, Range};
use super::events::{EventExtra, fire_event, fire_event_with};
use super::misc::BlobData;
use super::{Js, K_OBJ, Obj, Runtime, Window, ctor, getter, illegal, method, need_args, setter};
use crate::registry::{ImplKind, Registry};

// ---------------------------------------------------------------------------
// Objects
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TxState {
    Active,
    Inactive,
    Committing,
    Finished,
}

#[derive(Clone, Copy, Debug)]
enum Source {
    None,
    Store(u32),
    Index(u32),
    Cursor(u32),
}

#[derive(Clone, Copy, Debug)]
enum ResultVal {
    Undefined,
    Null,
    Handle(Handle),
    Obj(u32),
}

pub(crate) struct Req {
    open: bool,
    done: bool,
    result: ResultVal,
    error: Option<Handle>,
    source: Source,
    tx: Option<u32>,
    /// A cursor request: the cursor object.
    cursor: Option<u32>,
}

pub(crate) struct Db {
    conn: u64,
    meta: DbMeta,
    close_pending: bool,
    upgrade: Option<u32>,
}

pub(crate) struct Tx {
    id: u64,
    db: u32,
    mode: Mode,
    scope: Vec<String>,
    state: TxState,
    error: Option<Handle>,
    stores: HashMap<String, u32>,
    /// Requests whose event has not fired yet, in order.
    pending: VecDeque<u32>,
    durability: &'static str,
    /// Version change: the open request and the metadata before.
    open_req: Option<u32>,
    old_meta: Option<DbMeta>,
    /// The connection closed during the upgrade.
    closed_in_upgrade: bool,
    /// Building an index failed: abort instead of committing.
    doomed: Option<Arg>,
    /// Every store object of this transaction (deleted ones too).
    objs: Vec<u32>,
}

pub(crate) struct StoreObj {
    tx: u32,
    /// Name when the version change transaction started (`None`: created
    /// in it); used to revert on abort.
    orig: Option<String>,
    /// Every index object of this store (deleted ones too).
    all_indexes: Vec<u32>,
    meta: StoreMeta,
    deleted: bool,
    indexes: HashMap<String, u32>,
    key_path: Option<Handle>,
}

pub(crate) struct IndexObj {
    store: u32,
    /// Name when the version change transaction started (`None`: created
    /// in it).
    orig: Option<String>,
    meta: IndexMeta,
    deleted: bool,
    key_path: Option<Handle>,
}

pub(crate) struct Cur {
    req: u32,
    store: u32,
    index: Option<u32>,
    dir: Dir,
    key_only: bool,
    range: Range,
    pos: Option<(Key, Key)>,
    got_value: bool,
    key: Option<Handle>,
    primary: Option<Handle>,
    value: Option<Handle>,
}

pub(crate) struct Rec {
    key: Handle,
    primary: Handle,
    value: Handle,
}

/// IndexedDB objects of a window.
pub(crate) enum IdbObj {
    Factory,
    Request(Box<Req>),
    Database(Box<Db>),
    Transaction(Box<Tx>),
    Store(Box<StoreObj>),
    Index(Box<IndexObj>),
    Cursor(Box<Cur>),
    KeyRange(Range),
    Record(Box<Rec>),
    StringList(Vec<String>),
}

impl IdbObj {
    pub(crate) fn iface_name(&self) -> &'static str {
        match self {
            IdbObj::Factory => "IDBFactory",
            IdbObj::Request(r) if r.open => "IDBOpenDBRequest",
            IdbObj::Request(_) => "IDBRequest",
            IdbObj::Database(_) => "IDBDatabase",
            IdbObj::Transaction(_) => "IDBTransaction",
            IdbObj::Store(_) => "IDBObjectStore",
            IdbObj::Index(_) => "IDBIndex",
            IdbObj::Cursor(c) if c.key_only => "IDBCursor",
            IdbObj::Cursor(_) => "IDBCursorWithValue",
            IdbObj::KeyRange(_) => "IDBKeyRange",
            IdbObj::Record(_) => "IDBRecord",
            IdbObj::StringList(_) => "DOMStringList",
        }
    }
}

/// IndexedDB state of a window.
#[derive(Default)]
pub(crate) struct IdbSide {
    backend: RefCell<Option<IdbBackend>>,
    /// Transactions created since the last task boundary.
    created: RefCell<Vec<u32>>,
    /// Backend transaction id → object; connection id → database object.
    txns: RefCell<HashMap<u64, u32>>,
    conns: RefCell<HashMap<u64, u32>>,
    sink: RefCell<Option<Sink>>,
}

impl Window {
    /// The browser context's IndexedDB backend (set by the page layer).
    pub fn set_idb_backend(&self, b: Option<IdbBackend>) {
        *self.idb.backend.borrow_mut() = b;
    }

    pub fn idb_backend(&self) -> Option<IdbBackend> {
        self.idb.backend.borrow().clone()
    }
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn with<R>(w: &Window, id: u32, f: impl FnOnce(&mut IdbObj) -> R) -> Option<R> {
    w.with_obj(id, |o| match o {
        Obj::Idb(x) => Some(f(x)),
        _ => None,
    })
    .flatten()
}

macro_rules! accessor {
    ($name:ident, $var:ident, $ty:ty) => {
        fn $name<R>(w: &Window, id: u32, f: impl FnOnce(&mut $ty) -> R) -> Option<R> {
            with(w, id, |o| match o {
                IdbObj::$var(x) => Some(f(x)),
                _ => None,
            })
            .flatten()
        }
    };
}
accessor!(with_req, Request, Req);
accessor!(with_db, Database, Db);
accessor!(with_tx, Transaction, Tx);
accessor!(with_store, Store, StoreObj);
accessor!(with_index, Index, IndexObj);
accessor!(with_cur, Cursor, Cur);

fn new_idb(w: &Window, o: IdbObj) -> u32 {
    w.new_obj(Obj::Idb(Box::new(o)))
}

fn this_id(cx: &mut CallCtx<'_, '_, '_>) -> Result<u32, HostError> {
    match cx.this_host() {
        Some(h) if h.kind == K_OBJ => Ok(h.id),
        _ => Err(illegal()),
    }
}

fn this_of(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    ok: impl Fn(&IdbObj) -> bool,
) -> Result<u32, HostError> {
    let id = this_id(cx)?;
    if with(w, id, |o| ok(o)).unwrap_or(false) {
        Ok(id)
    } else {
        Err(illegal())
    }
}

fn exec_err(w: &Window, name: &str, op: &str, iface: &str, msg: &str) -> HostError {
    let m = format!("Failed to execute '{op}' on '{iface}': {msg}");
    if name == "TypeError" {
        HostError::TypeError(m)
    } else {
        w.rt.err(name, m)
    }
}

fn dom_exception(w: &Window, name: &str, msg: &str) -> Arg {
    match w.rt.dom().interface() {
        Some(id) => Arg::Construct(id, vec![Arg::String(msg.into()), Arg::String(name.into())]),
        None => Arg::String(msg.into()),
    }
}

fn sink_of(w: &Rc<Window>) -> Sink {
    if let Some(s) = w.idb.sink.borrow().clone() {
        return s;
    }
    let inbox = w.inbox();
    let s: Sink = Arc::new(move |ev: Ev| {
        let _ = inbox.send(Box::new(move |js, w| on_event(js, w, ev)));
    });
    *w.idb.sink.borrow_mut() = Some(s.clone());
    s
}

fn backend(w: &Window) -> Result<IdbBackend, HostError> {
    w.idb_backend()
        .ok_or_else(|| HostError::Error("IndexedDB is not available".into()))
}

/// `[EnforceRange] unsigned long` of an optional argument.
fn enforce_u32(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    i: usize,
    op: &str,
    iface: &str,
) -> Result<Option<u32>, HostError> {
    if cx.len() <= i || cx.arg_kind(i) == ValueKind::Undefined {
        return Ok(None);
    }
    let n = cx.arg_f64(i)?;
    if !n.is_finite() {
        return Err(exec_err(
            w,
            "TypeError",
            op,
            iface,
            "Value is not of type 'unsigned long'.",
        ));
    }
    let n = n.trunc();
    if !(0.0..=4_294_967_295.0).contains(&n) {
        return Err(exec_err(
            w,
            "TypeError",
            op,
            iface,
            "Value is outside the 'unsigned long' value range.",
        ));
    }
    Ok(Some(n as u32))
}

/// Sort names as JS does (UTF-16 code units).
fn sort_names(names: &mut [String]) {
    names.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
}

fn list_arg(w: &Window, mut names: Vec<String>) -> Arg {
    sort_names(&mut names);
    names.dedup();
    w.obj_arg(new_idb(w, IdbObj::StringList(names)))
}

fn key_range_of(w: &Window, h: Option<HostRef>) -> Option<Range> {
    let h = h.filter(|h| h.kind == K_OBJ)?;
    with(w, h.id, |o| match o {
        IdbObj::KeyRange(r) => Some(r.clone()),
        _ => None,
    })
    .flatten()
}

/// A query argument: a key range, a key, or (`null_ok`) nothing.
fn range_arg(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    i: usize,
    null_ok: bool,
    op: &str,
    iface: &str,
) -> Result<Range, HostError> {
    if cx.len() <= i || matches!(cx.arg_kind(i), ValueKind::Undefined | ValueKind::Null) {
        if null_ok {
            return Ok(Range::all());
        }
        return Err(exec_err(
            w,
            "DataError",
            op,
            iface,
            "No key or key range specified.",
        ));
    }
    let host = cx.arg_host(i);
    if let Some(r) = key_range_of(w, host) {
        return Ok(r);
    }
    let h = cx.arg_handle(i);
    let k = key::to_key(cx, h);
    cx.release(h);
    match k? {
        Some(k) => Ok(Range::only(k)),
        None => Err(exec_err(
            w,
            "DataError",
            op,
            iface,
            "The parameter is not a valid key.",
        )),
    }
}

fn key_arg(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    i: usize,
    op: &str,
    iface: &str,
) -> Result<Key, HostError> {
    let h = cx.arg_handle(i);
    let k = key::to_key(cx, h);
    cx.release(h);
    k?.ok_or_else(|| {
        exec_err(
            w,
            "DataError",
            op,
            iface,
            "The parameter is not a valid key.",
        )
    })
}

// ---------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------

/// Structured-clone `h` for storage. Blobs travel as bytes.
fn clone_value(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    h: Handle,
    op: &str,
    iface: &str,
) -> Result<Value, HostError> {
    let mut blobs: Vec<(Vec<u8>, String)> = Vec::new();
    let r = {
        let mut writer = |hr: HostRef, class: &str| -> Result<u32, String> {
            if hr.kind == K_OBJ {
                let blob = w
                    .with_obj(hr.id, |o| match o {
                        Obj::Blob(b) => Some((b.bytes.clone(), b.type_.clone())),
                        _ => None,
                    })
                    .flatten();
                if let Some(b) = blob {
                    blobs.push(b);
                    return Ok((blobs.len() - 1) as u32);
                }
            }
            Err(format!("{class} object could not be cloned."))
        };
        cx.serialize(h, &[], &mut writer)
    };
    match r {
        Ok(s) => Ok(Value {
            data: s.data,
            buffers: s.buffers,
            blobs,
        }),
        Err(Some(m)) => Err(exec_err(w, "DataCloneError", op, iface, &m)),
        Err(None) => Err(HostError::Thrown),
    }
}

fn host_entries(w: &Window, v: &Value) -> Vec<(InterfaceId, HostRef)> {
    v.blobs
        .iter()
        .map(|(bytes, type_)| {
            let id = w.new_obj(Obj::Blob(Box::new(BlobData {
                bytes: bytes.clone(),
                type_: type_.clone(),
            })));
            (w.rt.iface("Blob"), HostRef::new(K_OBJ, id))
        })
        .collect()
}

fn serialized(v: &Value) -> lb_js::Serialized {
    lb_js::Serialized {
        data: v.data.clone(),
        buffers: v.buffers.clone(),
    }
}

/// A stored value as a new JS value, with the primary key injected for
/// key generator stores.
fn read_value(
    js: &mut dyn Js,
    w: &Window,
    v: &Value,
    pk: &Key,
    inject: Option<&str>,
) -> Option<Handle> {
    let hosts = host_entries(w, v);
    let h = js.deserialize(&serialized(v), &hosts).ok()?;
    if let Some(p) = inject {
        key::inject(js, h, p, pk);
    }
    Some(h)
}

/// The clone of `h` in this realm (key paths are evaluated on it).
fn clone_in_realm(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    v: &Value,
) -> Result<Handle, HostError> {
    let hosts = host_entries(w, v);
    cx.deserialize(&serialized(v), &hosts)
        .map_err(HostError::Error)
}

// ---------------------------------------------------------------------------
// Transactions
// ---------------------------------------------------------------------------

fn tx_state(w: &Window, tx: u32) -> TxState {
    with_tx(w, tx, |t| t.state).unwrap_or(TxState::Finished)
}

/// The "transaction is active" check of a request method.
fn check_active(w: &Window, tx: u32, op: &str, iface: &str) -> Result<(), HostError> {
    match tx_state(w, tx) {
        TxState::Active => Ok(()),
        TxState::Inactive => Err(exec_err(
            w,
            "TransactionInactiveError",
            op,
            iface,
            "The transaction is not active.",
        )),
        _ => Err(exec_err(
            w,
            "TransactionInactiveError",
            op,
            iface,
            "The transaction has finished.",
        )),
    }
}

/// Run at each task boundary (before a task runs): transactions created by
/// the previous task become inactive and commit if they have no request.
pub(crate) fn task_boundary(w: &Window) {
    if w.idb.created.borrow().is_empty() {
        return;
    }
    let created: Vec<u32> = std::mem::take(&mut *w.idb.created.borrow_mut());
    for tx in created {
        let commit = with_tx(w, tx, |t| {
            if t.state == TxState::Active {
                t.state = TxState::Inactive;
            }
            t.state == TxState::Inactive && t.pending.is_empty()
        })
        .unwrap_or(false);
        if commit {
            start_commit(w, tx);
        }
    }
}

fn start_commit(w: &Window, tx: u32) {
    // A failed index build: the task queued with it aborts instead.
    if with_tx(w, tx, |t| t.doomed.is_some()).unwrap_or(false) {
        return;
    }
    let id = with_tx(w, tx, |t| {
        t.state = TxState::Committing;
        t.id
    });
    if let (Some(id), Some(b)) = (id, w.idb_backend()) {
        b.commit(id);
    }
}

/// A new request of transaction `tx`, placed with the backend.
fn place(w: &Rc<Window>, tx: u32, source: Source, op: Op) -> Result<u32, HostError> {
    let req = new_idb(
        w,
        IdbObj::Request(Box::new(Req {
            open: false,
            done: false,
            result: ResultVal::Undefined,
            error: None,
            source,
            tx: Some(tx),
            cursor: None,
        })),
    );
    let id = with_tx(w, tx, |t| {
        t.pending.push_back(req);
        t.id
    })
    .unwrap_or(0);
    backend(w)?.request(id, req, op);
    Ok(req)
}

/// Re-place a cursor's request (continue/advance).
fn replace_cursor(w: &Rc<Window>, tx: u32, req: u32, op: Op) -> Result<(), HostError> {
    with_req(w, req, |r| {
        r.done = false;
    });
    let id = with_tx(w, tx, |t| {
        t.pending.push_back(req);
        t.id
    })
    .unwrap_or(0);
    backend(w)?.request(id, req, op);
    Ok(())
}

/// Abort transaction `tx` with `error` (null for `abort()`): roll back now,
/// fire the request errors and `abort` in a task.
fn abort_tx(w: &Rc<Window>, tx: u32, error: Option<Arg>) {
    let Some((id, state, db, mode)) = with_tx(w, tx, |t| (t.id, t.state, t.db, t.mode)) else {
        return;
    };
    if state == TxState::Finished {
        return;
    }
    with_tx(w, tx, |t| {
        t.state = TxState::Finished;
        t.doomed = None;
    });
    if let Some(b) = w.idb_backend() {
        b.abort(id);
    }
    if mode == Mode::VersionChange {
        // The connection's view goes back to the old version at once.
        let old = with_tx(w, tx, |t| t.old_meta.clone()).flatten();
        if let Some(old) = old {
            revert_objects(w, tx, &old);
            with_db(w, db, |d| d.meta = old);
        }
    }
    let error = error.map(|e| {
        // Kept as an Arg until the task makes it a handle.
        e
    });
    let _ = w.inbox().send(Box::new(move |js, w| {
        fire_abort(js, w, tx, error);
    }));
}

/// Store and index objects of an aborted upgrade go back to the old
/// schema: created ones are deleted, deleted ones come back, names revert.
fn revert_objects(w: &Window, tx: u32, old: &DbMeta) {
    let objs = with_tx(w, tx, |t| t.objs.clone()).unwrap_or_default();
    for so in objs {
        let Some((orig, indexes)) = with_store(w, so, |s| (s.orig.clone(), s.all_indexes.clone()))
        else {
            continue;
        };
        let old_store = orig
            .as_ref()
            .and_then(|n| old.stores.iter().find(|s| s.name == *n).cloned());
        with_store(w, so, |s| match &old_store {
            Some(m) => {
                s.deleted = false;
                s.meta = m.clone();
            }
            None => s.deleted = true,
        });
        for ix in indexes {
            with_index(w, ix, |x| {
                let old_ix = old_store.as_ref().and_then(|m| {
                    x.orig
                        .as_ref()
                        .and_then(|n| m.indexes.iter().find(|i| i.name == *n).cloned())
                });
                match old_ix {
                    Some(m) => {
                        x.deleted = false;
                        x.meta = m;
                    }
                    None => x.deleted = true,
                }
            });
        }
    }
}

fn fire_abort(js: &mut dyn Js, w: &Rc<Window>, tx: u32, error: Option<Arg>) {
    let err_handle = error.and_then(|e| match e {
        Arg::Handle(h) => Some(h),
        other => js.make(other),
    });
    with_tx(w, tx, |t| t.error = err_handle);
    let pending: Vec<u32> = with_tx(w, tx, |t| t.pending.drain(..).collect()).unwrap_or_default();
    for req in pending {
        let e = js.make(dom_exception(
            w,
            "AbortError",
            "The transaction was aborted, so the request cannot be fulfilled.",
        ));
        with_req(w, req, |r| {
            r.done = true;
            r.result = ResultVal::Undefined;
            r.error = e;
        });
        fire_event(js, w, HostRef::new(K_OBJ, req), "error", true, true);
    }
    let db = with_tx(w, tx, |t| t.db).unwrap_or(0);
    with_db(w, db, |d| {
        if d.upgrade == Some(tx) {
            d.upgrade = None;
        }
    });
    fire_event(js, w, HostRef::new(K_OBJ, tx), "abort", true, false);
    // A version change transaction: the open request fails and the
    // connection is closed.
    let (open_req, closed, db) =
        with_tx(w, tx, |t| (t.open_req, t.closed_in_upgrade, t.db)).unwrap_or((None, false, 0));
    if let Some(req) = open_req {
        with_db(w, db, |d| d.close_pending = true);
        let msg = if closed {
            "The connection was closed."
        } else {
            "Version change transaction was aborted in upgradeneeded event handler."
        };
        let e = js.make(dom_exception(w, "AbortError", msg));
        with_req(w, req, |r| {
            r.done = true;
            r.result = ResultVal::Undefined;
            r.error = e;
            r.tx = None;
        });
        fire_event(js, w, HostRef::new(K_OBJ, req), "error", true, true);
    }
}

/// Dispatch `f` with transaction `tx` active; abort it if a listener
/// threw. Returns whatever `f` returns.
fn with_active<R>(
    js: &mut dyn Js,
    w: &Rc<Window>,
    tx: Option<u32>,
    f: impl FnOnce(&mut dyn Js) -> R,
) -> R {
    let before = w.uncaught.get();
    if let Some(t) = tx {
        with_tx(w, t, |x| {
            if x.state == TxState::Inactive {
                x.state = TxState::Active;
            }
        });
    }
    let r = f(js);
    if let Some(t) = tx {
        with_tx(w, t, |x| {
            if x.state == TxState::Active {
                x.state = TxState::Inactive;
            }
        });
        // After an explicit commit() an exception no longer aborts.
        if w.uncaught.get() != before
            && !matches!(tx_state(w, t), TxState::Finished | TxState::Committing)
        {
            let e = dom_exception(w, "AbortError", "Uncaught exception in event handler.");
            abort_tx(w, t, Some(e));
        }
    }
    r
}

/// After a request's event: commit when nothing is left.
fn after_request(w: &Rc<Window>, tx: u32) {
    let (commit, explicit) = with_tx(w, tx, |t| {
        (
            t.state == TxState::Inactive && t.pending.is_empty(),
            t.state == TxState::Committing && t.pending.is_empty(),
        )
    })
    .unwrap_or((false, false));
    if commit {
        start_commit(w, tx);
    } else if explicit && let (Some(id), Some(b)) = (with_tx(w, tx, |t| t.id), w.idb_backend()) {
        b.commit(id);
    }
}

// ---------------------------------------------------------------------------
// Backend events
// ---------------------------------------------------------------------------

fn on_event(js: &mut dyn Js, w: &Rc<Window>, ev: Ev) {
    match ev {
        Ev::Result {
            req,
            result,
            inject,
            ..
        } => on_result(js, w, req, result, inject),
        Ev::Complete { txn } => {
            let Some(tx) = w.idb.txns.borrow().get(&txn).copied() else {
                return;
            };
            let Some((db, mode, open_req)) = with_tx(w, tx, |t| {
                t.state = TxState::Finished;
                (t.db, t.mode, t.open_req)
            }) else {
                return;
            };
            if mode == Mode::VersionChange {
                with_db(w, db, |d| d.upgrade = None);
                if let Some(r) = open_req {
                    with_req(w, r, |r| r.tx = None);
                }
            }
            fire_event(js, w, HostRef::new(K_OBJ, tx), "complete", false, false);
        }
        Ev::VersionChange { conn, old, new } => {
            let db = w.idb.conns.borrow().get(&conn).copied();
            if let Some(db) = db
                && !with_db(w, db, |d| d.close_pending).unwrap_or(true)
            {
                fire_event_with(
                    js,
                    w,
                    HostRef::new(K_OBJ, db),
                    "IDBVersionChangeEvent",
                    "versionchange",
                    false,
                    false,
                    EventExtra::VersionChange { old, new },
                );
            }
            if let Some(b) = w.idb_backend() {
                b.version_change_handled(conn);
            }
        }
        Ev::Blocked { req, old, new } => {
            fire_event_with(
                js,
                w,
                HostRef::new(K_OBJ, req),
                "IDBVersionChangeEvent",
                "blocked",
                false,
                false,
                EventExtra::VersionChange { old, new },
            );
        }
        Ev::Upgrade {
            req,
            conn,
            txn,
            old,
            meta,
        } => {
            let new = meta.version;
            let mut old_meta = meta.clone();
            old_meta.version = old;
            let db = new_idb(
                w,
                IdbObj::Database(Box::new(Db {
                    conn,
                    meta: meta.clone(),
                    close_pending: false,
                    upgrade: None,
                })),
            );
            w.idb.conns.borrow_mut().insert(conn, db);
            let tx = new_idb(
                w,
                IdbObj::Transaction(Box::new(Tx {
                    id: txn,
                    db,
                    mode: Mode::VersionChange,
                    scope: meta.stores.iter().map(|s| s.name.clone()).collect(),
                    state: TxState::Inactive,
                    error: None,
                    stores: HashMap::new(),
                    pending: VecDeque::new(),
                    durability: "default",
                    open_req: Some(req),
                    old_meta: Some(old_meta),
                    closed_in_upgrade: false,
                    doomed: None,
                    objs: Vec::new(),
                })),
            );
            w.idb.txns.borrow_mut().insert(txn, tx);
            with_db(w, db, |d| d.upgrade = Some(tx));
            with_req(w, req, |r| {
                r.done = true;
                r.result = ResultVal::Obj(db);
                r.tx = Some(tx);
            });
            with_active(js, w, Some(tx), |js| {
                fire_event_with(
                    js,
                    w,
                    HostRef::new(K_OBJ, req),
                    "IDBVersionChangeEvent",
                    "upgradeneeded",
                    false,
                    false,
                    EventExtra::VersionChange {
                        old,
                        new: Some(new),
                    },
                );
            });
            after_request(w, tx);
        }
        Ev::Opened { req, conn, meta } => {
            let existing = w.idb.conns.borrow().get(&conn).copied();
            let db = match existing {
                Some(db) => {
                    with_db(w, db, |d| d.meta = meta.clone());
                    db
                }
                None => {
                    let db = new_idb(
                        w,
                        IdbObj::Database(Box::new(Db {
                            conn,
                            meta,
                            close_pending: false,
                            upgrade: None,
                        })),
                    );
                    w.idb.conns.borrow_mut().insert(conn, db);
                    db
                }
            };
            // An upgrade's connection the page closed: the request fails.
            if existing.is_some() && with_db(w, db, |d| d.close_pending).unwrap_or(false) {
                let e = js.make(dom_exception(w, "AbortError", "The connection was closed."));
                with_req(w, req, |r| {
                    r.done = true;
                    r.result = ResultVal::Undefined;
                    r.error = e;
                    r.tx = None;
                });
                fire_event(js, w, HostRef::new(K_OBJ, req), "error", true, true);
                return;
            }
            with_req(w, req, |r| {
                r.done = true;
                r.result = ResultVal::Obj(db);
                r.tx = None;
            });
            fire_event(js, w, HostRef::new(K_OBJ, req), "success", false, false);
        }
        Ev::OpenFailed { req, name, message } => {
            let e = js.make(dom_exception(w, name, &message));
            with_req(w, req, |r| {
                r.done = true;
                r.result = ResultVal::Undefined;
                r.error = e;
            });
            fire_event(js, w, HostRef::new(K_OBJ, req), "error", true, true);
        }
        Ev::Deleted { req, old } => {
            with_req(w, req, |r| {
                r.done = true;
                r.result = ResultVal::Undefined;
            });
            fire_event_with(
                js,
                w,
                HostRef::new(K_OBJ, req),
                "IDBVersionChangeEvent",
                "success",
                false,
                false,
                EventExtra::VersionChange { old, new: None },
            );
        }
    }
}

fn keys_arg(keys: &[Key]) -> Arg {
    Arg::Array(keys.iter().map(Key::to_arg).collect())
}

fn on_result(js: &mut dyn Js, w: &Rc<Window>, req: u32, result: OpResult, inject: Option<String>) {
    let Some((tx, cursor, done)) = with_req(w, req, |r| (r.tx, r.cursor, r.done)) else {
        return;
    };
    let Some(tx) = tx else { return };
    if done || tx_state(w, tx) == TxState::Finished {
        return;
    }
    // Only the oldest pending request may complete (results come in order).
    let is_pending = with_tx(w, tx, |t| t.pending.contains(&req)).unwrap_or(false);
    if !is_pending {
        return;
    }
    let mut error = None;
    let value = match result {
        OpResult::Error { name, message } => {
            error = js.make(dom_exception(w, name, &message));
            ResultVal::Undefined
        }
        OpResult::Undefined => ResultVal::Undefined,
        OpResult::Key(k) => js
            .make(k.to_arg())
            .map_or(ResultVal::Undefined, ResultVal::Handle),
        OpResult::Count(n) => js
            .make(Arg::Number(n as f64))
            .map_or(ResultVal::Undefined, ResultVal::Handle),
        OpResult::Keys(ks) => js
            .make(keys_arg(&ks))
            .map_or(ResultVal::Undefined, ResultVal::Handle),
        OpResult::Value(None) => ResultVal::Undefined,
        OpResult::Value(Some((v, pk))) => read_value(js, w, &v, &pk, inject.as_deref())
            .map_or(ResultVal::Undefined, ResultVal::Handle),
        OpResult::Values(vs) => {
            let mut items = Vec::new();
            for (v, pk) in &vs {
                match read_value(js, w, v, pk, inject.as_deref()) {
                    Some(h) => items.push(Arg::Handle(h)),
                    None => items.push(Arg::Undefined),
                }
            }
            let h = js.make(Arg::Array(items.clone()));
            for i in items {
                if let Arg::Handle(h) = i {
                    js.release(h);
                }
            }
            h.map_or(ResultVal::Undefined, ResultVal::Handle)
        }
        OpResult::Records(rs) => {
            let mut items = Vec::new();
            for (k, pk, v) in &rs {
                let (Some(kh), Some(ph)) = (js.make(k.to_arg()), js.make(pk.to_arg())) else {
                    continue;
                };
                let vh =
                    read_value(js, w, v, pk, inject.as_deref()).or_else(|| js.make(Arg::Undefined));
                let Some(vh) = vh else { continue };
                let id = new_idb(
                    w,
                    IdbObj::Record(Box::new(Rec {
                        key: kh,
                        primary: ph,
                        value: vh,
                    })),
                );
                items.push(w.obj_arg(id));
            }
            js.make(Arg::Array(items))
                .map_or(ResultVal::Undefined, ResultVal::Handle)
        }
        OpResult::Cursor(pos) => {
            let Some(cid) = cursor else {
                return;
            };
            match pos {
                None => {
                    with_cur(w, cid, |c| {
                        c.got_value = false;
                    });
                    ResultVal::Null
                }
                Some(p) => {
                    let value = match &p.value {
                        Some(v) => read_value(js, w, v, &p.primary_key, inject.as_deref()),
                        None => None,
                    };
                    let kh = js.make(p.key.to_arg());
                    let ph = js.make(p.primary_key.to_arg());
                    with_cur(w, cid, |c| {
                        c.pos = Some((p.key.clone(), p.primary_key.clone()));
                        c.got_value = true;
                        c.key = kh;
                        c.primary = ph;
                        c.value = value;
                    });
                    ResultVal::Obj(cid)
                }
            }
        }
    };
    with_tx(w, tx, |t| t.pending.retain(|r| *r != req));
    with_req(w, req, |r| {
        r.done = true;
        r.result = value;
        r.error = error;
    });
    let target = HostRef::new(K_OBJ, req);
    if let Some(e) = error {
        let not_canceled = with_active(js, w, Some(tx), |js| {
            fire_event(js, w, target, "error", true, true)
        });
        if not_canceled && tx_state(w, tx) != TxState::Finished {
            abort_tx(w, tx, Some(Arg::Handle(e)));
            return;
        }
    } else {
        with_active(js, w, Some(tx), |js| {
            fire_event(js, w, target, "success", false, false)
        });
    }
    after_request(w, tx);
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    register_factory(reg, rt);
    register_request(reg, rt);
    register_database(reg, rt);
    register_transaction(reg, rt);
    register_store(reg, rt);
    register_index(reg, rt);
    register_cursor(reg, rt);
    register_key_range(reg, rt);
    register_misc(reg, rt);
}

fn opaque(w: &Window) -> bool {
    w.origin_string() == "null"
}

fn register_factory(reg: &mut Registry, rt: &Runtime) {
    for iface in ["Window", "WorkerGlobalScope"] {
        getter(reg, rt, iface, "indexedDB", |cx, w| {
            let id = w.cached_obj((0, 0, "indexedDB", String::new()), || {
                Obj::Idb(Box::new(IdbObj::Factory))
            });
            cx.ret(w.obj_arg(id))
        });
    }
    let is_factory = |o: &IdbObj| matches!(o, IdbObj::Factory);
    method(reg, rt, "IDBFactory", "open", move |cx, w| {
        this_of(cx, w, is_factory)?;
        need_args(cx, 1, "open", "IDBFactory")?;
        let name = cx.arg_string(0)?;
        let version = if cx.len() > 1 && cx.arg_kind(1) != ValueKind::Undefined {
            let n = cx.arg_f64(1)?;
            if n.is_nan() || n.is_infinite() {
                return Err(exec_err(
                    w,
                    "TypeError",
                    "open",
                    "IDBFactory",
                    "Value is not of type 'unsigned long long'.",
                ));
            }
            let n = n.trunc();
            if !(0.0..=9_007_199_254_740_991.0).contains(&n) {
                return Err(exec_err(
                    w,
                    "TypeError",
                    "open",
                    "IDBFactory",
                    "Value is outside the 'unsigned long long' value range.",
                ));
            }
            if n == 0.0 {
                return Err(exec_err(
                    w,
                    "TypeError",
                    "open",
                    "IDBFactory",
                    "The version provided must not be 0.",
                ));
            }
            Some(n as u64)
        } else {
            None
        };
        if opaque(w) {
            return Err(exec_err(
                w,
                "SecurityError",
                "open",
                "IDBFactory",
                "access to the Indexed Database API is denied in this context.",
            ));
        }
        let b = backend(w)?;
        let req = new_idb(
            w,
            IdbObj::Request(Box::new(Req {
                open: true,
                done: false,
                result: ResultVal::Undefined,
                error: None,
                source: Source::None,
                tx: None,
                cursor: None,
            })),
        );
        b.open(&w.origin_string(), &name, version, req, sink_of(w));
        cx.ret(w.obj_arg(req))
    });
    method(reg, rt, "IDBFactory", "deleteDatabase", move |cx, w| {
        this_of(cx, w, is_factory)?;
        need_args(cx, 1, "deleteDatabase", "IDBFactory")?;
        let name = cx.arg_string(0)?;
        if opaque(w) {
            return Err(exec_err(
                w,
                "SecurityError",
                "deleteDatabase",
                "IDBFactory",
                "access to the Indexed Database API is denied in this context.",
            ));
        }
        let b = backend(w)?;
        let req = new_idb(
            w,
            IdbObj::Request(Box::new(Req {
                open: true,
                done: false,
                result: ResultVal::Undefined,
                error: None,
                source: Source::None,
                tx: None,
                cursor: None,
            })),
        );
        b.delete(&w.origin_string(), &name, req, sink_of(w));
        cx.ret(w.obj_arg(req))
    });
    method(reg, rt, "IDBFactory", "cmp", move |cx, w| {
        this_of(cx, w, is_factory)?;
        need_args(cx, 2, "cmp", "IDBFactory")?;
        let a = key_arg(cx, w, 0, "cmp", "IDBFactory")?;
        let b = key_arg(cx, w, 1, "cmp", "IDBFactory")?;
        cx.ret(match a.cmp(&b) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        })
    });
    method(reg, rt, "IDBFactory", "databases", move |cx, w| {
        this_of(cx, w, is_factory)?;
        let r = cx.ret_new_promise()?;
        if opaque(w) {
            return cx.reject_promise(
                r,
                exec_err(
                    w,
                    "SecurityError",
                    "databases",
                    "IDBFactory",
                    "access to the Indexed Database API is denied in this context.",
                ),
            );
        }
        let list = backend(w)?.databases(&w.origin_string());
        let json = format!(
            "[{}]",
            list.iter()
                .map(|(n, v)| format!(
                    "{{\"name\":{},\"version\":{v}}}",
                    serde_json::to_string(n).unwrap_or_default()
                ))
                .collect::<Vec<_>>()
                .join(",")
        );
        // Chrome answers from the backend: a later task.
        let _ = w.inbox().send(Box::new(move |js, _w| {
            js.resolve(r, Arg::Json(json));
        }));
        Ok(())
    });
}

fn is_req(o: &IdbObj) -> bool {
    matches!(o, IdbObj::Request(_))
}

fn register_request(reg: &mut Registry, rt: &Runtime) {
    getter(reg, rt, "IDBRequest", "result", |cx, w| {
        let id = this_of(cx, w, is_req)?;
        let (done, result) =
            with_req(w, id, |r| (r.done, r.result)).unwrap_or((true, ResultVal::Undefined));
        if !done {
            return Err(w.rt.err(
                "InvalidStateError",
                "Failed to read the 'result' property from 'IDBRequest': The request has not finished.",
            ));
        }
        cx.ret(match result {
            ResultVal::Undefined => Arg::Undefined,
            ResultVal::Null => Arg::Null,
            ResultVal::Handle(h) => Arg::Handle(h),
            ResultVal::Obj(o) => w.obj_arg(o),
        })
    });
    getter(reg, rt, "IDBRequest", "error", |cx, w| {
        let id = this_of(cx, w, is_req)?;
        let (done, e) = with_req(w, id, |r| (r.done, r.error)).unwrap_or((true, None));
        if !done {
            return Err(w.rt.err(
                "InvalidStateError",
                "Failed to read the 'error' property from 'IDBRequest': The request has not finished.",
            ));
        }
        cx.ret(e.map_or(Arg::Null, Arg::Handle))
    });
    getter(reg, rt, "IDBRequest", "source", |cx, w| {
        let id = this_of(cx, w, is_req)?;
        let s = with_req(w, id, |r| r.source).unwrap_or(Source::None);
        cx.ret(match s {
            Source::None => Arg::Null,
            Source::Store(o) | Source::Index(o) | Source::Cursor(o) => w.obj_arg(o),
        })
    });
    getter(reg, rt, "IDBRequest", "transaction", |cx, w| {
        let id = this_of(cx, w, is_req)?;
        let t = with_req(w, id, |r| r.tx).flatten();
        cx.ret(t.map_or(Arg::Null, |t| w.obj_arg(t)))
    });
    getter(reg, rt, "IDBRequest", "readyState", |cx, w| {
        let id = this_of(cx, w, is_req)?;
        let done = with_req(w, id, |r| r.done).unwrap_or(true);
        cx.ret(if done { "done" } else { "pending" })
    });
}

fn is_db(o: &IdbObj) -> bool {
    matches!(o, IdbObj::Database(_))
}

fn store_names(meta: &DbMeta) -> Vec<String> {
    meta.stores.iter().map(|s| s.name.clone()).collect()
}

/// The running upgrade transaction of a connection, active or not.
fn upgrade_of(w: &Window, db: u32) -> Option<u32> {
    with_db(w, db, |d| d.upgrade).flatten()
}

/// Checks of schema methods: an upgrade is running and active.
fn check_upgrade(w: &Window, db: u32, op: &str, iface: &str) -> Result<u32, HostError> {
    let Some(tx) = upgrade_of(w, db) else {
        return Err(exec_err(
            w,
            "InvalidStateError",
            op,
            iface,
            "The database is not running a version change transaction.",
        ));
    };
    check_active(w, tx, op, iface)?;
    Ok(tx)
}

fn register_database(reg: &mut Registry, rt: &Runtime) {
    getter(reg, rt, "IDBDatabase", "name", |cx, w| {
        let id = this_of(cx, w, is_db)?;
        cx.ret(with_db(w, id, |d| d.meta.name.clone()).unwrap_or_default())
    });
    getter(reg, rt, "IDBDatabase", "version", |cx, w| {
        let id = this_of(cx, w, is_db)?;
        cx.ret(with_db(w, id, |d| d.meta.version as f64).unwrap_or(0.0))
    });
    getter(reg, rt, "IDBDatabase", "objectStoreNames", |cx, w| {
        let id = this_of(cx, w, is_db)?;
        let names = with_db(w, id, |d| store_names(&d.meta)).unwrap_or_default();
        cx.ret(list_arg(w, names))
    });
    method(reg, rt, "IDBDatabase", "close", |cx, w| {
        let id = this_of(cx, w, is_db)?;
        let (conn, upgrade) = with_db(w, id, |d| {
            d.close_pending = true;
            (d.conn, d.upgrade)
        })
        .unwrap_or((0, None));
        // During an upgrade the transaction still commits; the open
        // request then fails (spec, WPT `close-in-upgradeneeded`).
        if let Some(tx) = upgrade
            && tx_state(w, tx) != TxState::Finished
        {
            with_tx(w, tx, |t| t.closed_in_upgrade = true);
        }
        if let Some(b) = w.idb_backend() {
            b.close(conn);
        }
        cx.ret(Arg::Undefined)
    });
    method(reg, rt, "IDBDatabase", "transaction", |cx, w| {
        let id = this_of(cx, w, is_db)?;
        need_args(cx, 1, "transaction", "IDBDatabase")?;
        // storeNames: a string or a sequence of strings.
        let names: Vec<String> = if cx.arg_kind(0) == ValueKind::Object {
            let h = cx.arg_handle(0);
            let r = key::key_path_arg(cx, h, false);
            cx.release(h);
            match r? {
                Some(KeyPath::Array(a)) => a,
                Some(KeyPath::String(s)) => vec![s],
                None => Vec::new(),
            }
        } else {
            vec![cx.arg_string(0)?]
        };
        let mode = if cx.len() > 1 && cx.arg_kind(1) != ValueKind::Undefined {
            let m = cx.arg_string(1)?;
            match m.as_str() {
                "readonly" => Mode::ReadOnly,
                "readwrite" => Mode::ReadWrite,
                "versionchange" => Mode::VersionChange,
                other => {
                    return Err(exec_err(
                        w,
                        "TypeError",
                        "transaction",
                        "IDBDatabase",
                        &format!(
                            "The provided value '{other}' is not a valid enum value of type IDBTransactionMode."
                        ),
                    ));
                }
            }
        } else {
            Mode::ReadOnly
        };
        let mut durability = "default";
        if cx.len() > 2 && cx.arg_kind(2) == ValueKind::Object {
            let h = cx.arg_handle(2);
            let d = cx.get_property(h, "durability")?;
            cx.release(h);
            if cx.handle_kind(d) != ValueKind::Undefined {
                let s = cx.handle_string(d)?;
                durability = match s.as_str() {
                    "default" => "default",
                    "strict" => "strict",
                    "relaxed" => "relaxed",
                    other => {
                        cx.release(d);
                        return Err(exec_err(
                            w,
                            "TypeError",
                            "transaction",
                            "IDBDatabase",
                            &format!(
                                "Failed to read the 'durability' property from 'IDBTransactionOptions': The provided value '{other}' is not a valid enum value of type IDBTransactionDurability."
                            ),
                        ));
                    }
                };
            }
            cx.release(d);
        }
        let (meta, upgrade, closing, conn) = with_db(w, id, |d| {
            (d.meta.clone(), d.upgrade, d.close_pending, d.conn)
        })
        .ok_or_else(illegal)?;
        if upgrade.is_some_and(|t| tx_state(w, t) != TxState::Finished) {
            return Err(exec_err(
                w,
                "InvalidStateError",
                "transaction",
                "IDBDatabase",
                "A version change transaction is running.",
            ));
        }
        if closing {
            return Err(exec_err(
                w,
                "InvalidStateError",
                "transaction",
                "IDBDatabase",
                "The database connection is closing.",
            ));
        }
        let mut scope = names;
        scope.sort();
        scope.dedup();
        let existing = store_names(&meta);
        if scope.iter().any(|s| !existing.contains(s)) {
            return Err(exec_err(
                w,
                "NotFoundError",
                "transaction",
                "IDBDatabase",
                "One of the specified object stores was not found.",
            ));
        }
        if scope.is_empty() {
            return Err(exec_err(
                w,
                "InvalidAccessError",
                "transaction",
                "IDBDatabase",
                "The storeNames parameter was empty.",
            ));
        }
        if mode == Mode::VersionChange {
            return Err(exec_err(
                w,
                "TypeError",
                "transaction",
                "IDBDatabase",
                "The mode provided ('versionchange') is not one of 'readonly' or 'readwrite'.",
            ));
        }
        let b = backend(w)?;
        let txn = b.transaction(conn, scope.clone(), mode, sink_of(w));
        let tx = new_idb(
            w,
            IdbObj::Transaction(Box::new(Tx {
                id: txn,
                db: id,
                mode,
                scope,
                state: TxState::Active,
                error: None,
                stores: HashMap::new(),
                pending: VecDeque::new(),
                durability,
                open_req: None,
                old_meta: None,
                closed_in_upgrade: false,
                doomed: None,
                objs: Vec::new(),
            })),
        );
        w.idb.txns.borrow_mut().insert(txn, tx);
        w.idb.created.borrow_mut().push(tx);
        cx.ret(w.obj_arg(tx))
    });
    method(reg, rt, "IDBDatabase", "createObjectStore", |cx, w| {
        let id = this_of(cx, w, is_db)?;
        need_args(cx, 1, "createObjectStore", "IDBDatabase")?;
        let name = cx.arg_string(0)?;
        let (op, iface) = ("createObjectStore", "IDBDatabase");
        let tx = check_upgrade(w, id, op, iface)?;
        let mut key_path = None;
        let mut auto = false;
        if cx.len() > 1 && cx.arg_kind(1) == ValueKind::Object {
            let h = cx.arg_handle(1);
            let kp = cx.get_property(h, "keyPath")?;
            let r = key::key_path_arg(cx, kp, true);
            cx.release(kp);
            let ai = cx.get_property(h, "autoIncrement")?;
            auto = matches!(cx.handle_value(ai), JsValue::Bool(true))
                || matches!(cx.handle_value(ai), JsValue::Number(n) if n != 0.0 && !n.is_nan())
                || matches!(cx.handle_value(ai), JsValue::String(s) if !s.is_empty())
                || matches!(cx.handle_value(ai), JsValue::Json(_));
            cx.release(ai);
            cx.release(h);
            key_path = r?;
        }
        if let Some(kp) = &key_path
            && !kp.is_valid()
        {
            return Err(exec_err(
                w,
                "SyntaxError",
                op,
                iface,
                "The keyPath option is not a valid key path.",
            ));
        }
        let meta = with_db(w, id, |d| d.meta.clone()).ok_or_else(illegal)?;
        if meta.stores.iter().any(|s| s.name == name) {
            return Err(exec_err(
                w,
                "ConstraintError",
                op,
                iface,
                "An object store with the specified name already exists.",
            ));
        }
        if auto
            && key_path
                .as_ref()
                .is_some_and(|k| k.is_array() || k.is_empty_string())
        {
            return Err(exec_err(
                w,
                "InvalidAccessError",
                op,
                iface,
                "The autoIncrement option was set but the keyPath option was empty or an array.",
            ));
        }
        let smeta = StoreMeta {
            name: name.clone(),
            key_path,
            auto_increment: auto,
            indexes: Vec::new(),
        };
        let txn = with_tx(w, tx, |t| t.id).unwrap_or(0);
        let _ = backend(w)?.schema(txn, Schema::CreateStore(smeta.clone()));
        with_db(w, id, |d| {
            d.meta.stores.push(smeta.clone());
            d.meta
                .stores
                .sort_by(|a, b| a.name.encode_utf16().cmp(b.name.encode_utf16()));
        });
        with_tx(w, tx, |t| {
            if !t.scope.contains(&name) {
                t.scope.push(name.clone());
                t.scope.sort();
            }
        });
        let so = store_obj(w, tx, &smeta);
        cx.ret(w.obj_arg(so))
    });
    method(reg, rt, "IDBDatabase", "deleteObjectStore", |cx, w| {
        let id = this_of(cx, w, is_db)?;
        need_args(cx, 1, "deleteObjectStore", "IDBDatabase")?;
        let name = cx.arg_string(0)?;
        let tx = check_upgrade(w, id, "deleteObjectStore", "IDBDatabase")?;
        let exists =
            with_db(w, id, |d| d.meta.stores.iter().any(|s| s.name == name)).unwrap_or(false);
        if !exists {
            return Err(exec_err(
                w,
                "NotFoundError",
                "deleteObjectStore",
                "IDBDatabase",
                "The specified object store was not found.",
            ));
        }
        let txn = with_tx(w, tx, |t| t.id).unwrap_or(0);
        let _ = backend(w)?.schema(txn, Schema::DeleteStore(name.clone()));
        with_db(w, id, |d| d.meta.stores.retain(|s| s.name != name));
        let so = with_tx(w, tx, |t| {
            t.scope.retain(|s| *s != name);
            t.stores.remove(&name)
        })
        .flatten();
        if let Some(so) = so {
            let idx: Vec<u32> = with_store(w, so, |s| {
                s.deleted = true;
                s.indexes.values().copied().collect()
            })
            .unwrap_or_default();
            for i in idx {
                with_index(w, i, |x| x.deleted = true);
            }
        }
        cx.ret(Arg::Undefined)
    });
}

/// The store object of `meta` in transaction `tx` (one per name).
fn store_obj(w: &Window, tx: u32, meta: &StoreMeta) -> u32 {
    if let Some(id) = with_tx(w, tx, |t| t.stores.get(&meta.name).copied()).flatten() {
        return id;
    }
    let orig = with_tx(w, tx, |t| match &t.old_meta {
        Some(old) => old
            .stores
            .iter()
            .any(|s| s.name == meta.name)
            .then(|| meta.name.clone()),
        None => Some(meta.name.clone()),
    })
    .flatten();
    let id = new_idb(
        w,
        IdbObj::Store(Box::new(StoreObj {
            tx,
            orig,
            all_indexes: Vec::new(),
            meta: meta.clone(),
            deleted: false,
            indexes: HashMap::new(),
            key_path: None,
        })),
    );
    with_tx(w, tx, |t| {
        t.stores.insert(meta.name.clone(), id);
        t.objs.push(id);
    });
    id
}

fn is_tx(o: &IdbObj) -> bool {
    matches!(o, IdbObj::Transaction(_))
}

fn register_transaction(reg: &mut Registry, rt: &Runtime) {
    getter(reg, rt, "IDBTransaction", "objectStoreNames", |cx, w| {
        let id = this_of(cx, w, is_tx)?;
        let (scope, db, mode) =
            with_tx(w, id, |t| (t.scope.clone(), t.db, t.mode)).ok_or_else(illegal)?;
        let names = if mode == Mode::VersionChange {
            with_db(w, db, |d| store_names(&d.meta)).unwrap_or(scope)
        } else {
            scope
        };
        cx.ret(list_arg(w, names))
    });
    getter(reg, rt, "IDBTransaction", "mode", |cx, w| {
        let id = this_of(cx, w, is_tx)?;
        cx.ret(with_tx(w, id, |t| t.mode.as_str()).unwrap_or("readonly"))
    });
    getter(reg, rt, "IDBTransaction", "durability", |cx, w| {
        let id = this_of(cx, w, is_tx)?;
        cx.ret(with_tx(w, id, |t| t.durability).unwrap_or("default"))
    });
    getter(reg, rt, "IDBTransaction", "db", |cx, w| {
        let id = this_of(cx, w, is_tx)?;
        let db = with_tx(w, id, |t| t.db).ok_or_else(illegal)?;
        cx.ret(w.obj_arg(db))
    });
    getter(reg, rt, "IDBTransaction", "error", |cx, w| {
        let id = this_of(cx, w, is_tx)?;
        let e = with_tx(w, id, |t| t.error).flatten();
        cx.ret(e.map_or(Arg::Null, Arg::Handle))
    });
    method(reg, rt, "IDBTransaction", "objectStore", |cx, w| {
        let id = this_of(cx, w, is_tx)?;
        need_args(cx, 1, "objectStore", "IDBTransaction")?;
        let name = cx.arg_string(0)?;
        let (state, scope, db) =
            with_tx(w, id, |t| (t.state, t.scope.clone(), t.db)).ok_or_else(illegal)?;
        if matches!(state, TxState::Finished | TxState::Committing) {
            return Err(exec_err(
                w,
                "InvalidStateError",
                "objectStore",
                "IDBTransaction",
                "The transaction has finished.",
            ));
        }
        let meta = with_db(w, db, |d| {
            d.meta.stores.iter().find(|s| s.name == name).cloned()
        })
        .flatten();
        let Some(meta) = meta.filter(|_| scope.contains(&name)) else {
            return Err(exec_err(
                w,
                "NotFoundError",
                "objectStore",
                "IDBTransaction",
                "The specified object store was not found.",
            ));
        };
        let so = store_obj(w, id, &meta);
        cx.ret(w.obj_arg(so))
    });
    method(reg, rt, "IDBTransaction", "abort", |cx, w| {
        let id = this_of(cx, w, is_tx)?;
        let state = tx_state(w, id);
        if matches!(state, TxState::Finished | TxState::Committing) {
            return Err(exec_err(
                w,
                "InvalidStateError",
                "abort",
                "IDBTransaction",
                "The transaction has finished.",
            ));
        }
        abort_tx(w, id, None);
        cx.ret(Arg::Undefined)
    });
    method(reg, rt, "IDBTransaction", "commit", |cx, w| {
        let id = this_of(cx, w, is_tx)?;
        match tx_state(w, id) {
            TxState::Finished | TxState::Committing => {
                return Err(exec_err(
                    w,
                    "InvalidStateError",
                    "commit",
                    "IDBTransaction",
                    "The transaction has finished.",
                ));
            }
            TxState::Inactive => {
                return Err(exec_err(
                    w,
                    "InvalidStateError",
                    "commit",
                    "IDBTransaction",
                    "The transaction is not active.",
                ));
            }
            TxState::Active => {}
        }
        // Requests already placed still fire (an error among them aborts);
        // the backend commits after the last one.
        let (txn, pending) = with_tx(w, id, |t| {
            t.state = TxState::Committing;
            (t.id, t.pending.len())
        })
        .unwrap_or((0, 0));
        if pending == 0 {
            backend(w)?.commit(txn);
        }
        cx.ret(Arg::Undefined)
    });
}

fn is_store(o: &IdbObj) -> bool {
    matches!(o, IdbObj::Store(_))
}

/// Common checks of store request methods: deleted, active, read-only.
fn store_checks(w: &Window, so: u32, op: &str, write: bool) -> Result<(u32, StoreMeta), HostError> {
    let (tx, deleted, meta) =
        with_store(w, so, |s| (s.tx, s.deleted, s.meta.clone())).ok_or_else(illegal)?;
    if deleted {
        return Err(exec_err(
            w,
            "InvalidStateError",
            op,
            "IDBObjectStore",
            "The object store has been deleted.",
        ));
    }
    check_active(w, tx, op, "IDBObjectStore")?;
    if write && with_tx(w, tx, |t| t.mode == Mode::ReadOnly).unwrap_or(false) {
        return Err(exec_err(
            w,
            "ReadOnlyError",
            op,
            "IDBObjectStore",
            "The transaction is read-only.",
        ));
    }
    Ok((tx, meta))
}

/// Current index metadata of a store (from the connection).
fn store_indexes(w: &Window, so: u32) -> Vec<IndexMeta> {
    let (tx, name) =
        with_store(w, so, |s| (s.tx, s.meta.name.clone())).unwrap_or((0, String::new()));
    let db = with_tx(w, tx, |t| t.db).unwrap_or(0);
    with_db(w, db, |d| {
        d.meta
            .stores
            .iter()
            .find(|s| s.name == name)
            .map(|s| s.indexes.clone())
    })
    .flatten()
    .unwrap_or_default()
}

/// Run `f` (a structured clone, which may run getters) with transaction
/// `tx` inactive, as the spec requires.
fn inactive_during<R>(w: &Window, tx: u32, f: impl FnOnce() -> R) -> R {
    with_tx(w, tx, |t| t.state = TxState::Inactive);
    let r = f();
    with_tx(w, tx, |t| {
        if t.state == TxState::Inactive {
            t.state = TxState::Active;
        }
    });
    r
}

/// `put`/`add` on a store (also `IDBCursor.update` with `key` given).
fn put_impl(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Rc<Window>,
    so: u32,
    op: &str,
    add: bool,
) -> Result<u32, HostError> {
    need_args(cx, 1, op, "IDBObjectStore")?;
    let (tx, meta) = store_checks(w, so, op, true)?;
    let iface = "IDBObjectStore";
    let has_key = cx.len() > 1 && cx.arg_kind(1) != ValueKind::Undefined;
    if meta.key_path.is_some() && has_key {
        return Err(exec_err(
            w,
            "DataError",
            op,
            iface,
            "The object store uses in-line keys and the key parameter was provided.",
        ));
    }
    if meta.key_path.is_none() && !meta.auto_increment && !has_key {
        return Err(exec_err(
            w,
            "DataError",
            op,
            iface,
            "The object store uses out-of-line keys and has no key generator and the key parameter was not provided.",
        ));
    }
    let explicit = if has_key {
        Some(key_arg(cx, w, 1, op, iface)?)
    } else {
        None
    };
    let vh = cx.arg_handle(0);
    let value = inactive_during(w, tx, || clone_value(cx, w, vh, op, iface));
    cx.release(vh);
    let value = value?;
    let clone = clone_in_realm(cx, w, &value)?;
    let r = put_keys(cx, w, so, &meta, &clone, explicit, op);
    cx.release(clone);
    let (key, index_keys, pk_indexes) = r?;
    place(
        w,
        tx,
        Source::Store(so),
        Op::Put {
            store: meta.name.clone(),
            value,
            key,
            add,
            index_keys,
            pk_indexes,
        },
    )
}

type PutKeys = (
    Option<Key>,
    Vec<(String, Vec<Key>)>,
    Vec<(String, Option<Vec<Option<Key>>>)>,
);

/// The primary key (`None`: generated) and index keys of a value.
fn put_keys(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    so: u32,
    meta: &StoreMeta,
    clone: &Handle,
    explicit: Option<Key>,
    op: &str,
) -> Result<PutKeys, HostError> {
    let iface = "IDBObjectStore";
    let key = match (&meta.key_path, explicit) {
        (_, Some(k)) => Some(k),
        (Some(kp), None) => match key::eval_key(cx, *clone, kp)? {
            Ok(Some(k)) => Some(k),
            Ok(None) => {
                return Err(exec_err(
                    w,
                    "DataError",
                    op,
                    iface,
                    "Evaluating the object store's key path yielded a value that is not a valid key.",
                ));
            }
            Err(()) if meta.auto_increment => {
                let KeyPath::String(p) = kp else {
                    return Err(illegal());
                };
                if !key::can_inject(cx, *clone, p)? {
                    return Err(exec_err(
                        w,
                        "DataError",
                        op,
                        iface,
                        "A generated key could not be inserted into the value.",
                    ));
                }
                None
            }
            Err(()) => {
                return Err(exec_err(
                    w,
                    "DataError",
                    op,
                    iface,
                    "Evaluating the object store's key path did not yield a value.",
                ));
            }
        },
        (None, None) => None,
    };
    let mut index_keys = Vec::new();
    let mut pk_indexes = Vec::new();
    for ix in store_indexes(w, so) {
        if key.is_none()
            && let Some(KeyPath::String(p)) = &meta.key_path
        {
            match &ix.key_path {
                KeyPath::String(ip) if ip == p => {
                    pk_indexes.push((ix.name.clone(), None));
                    continue;
                }
                KeyPath::Array(ps) if ps.contains(p) => {
                    let mut parts = Vec::new();
                    let mut ok = true;
                    for q in ps {
                        if q == p {
                            parts.push(None);
                            continue;
                        }
                        match key::eval_key(cx, *clone, &KeyPath::String(q.clone()))? {
                            Ok(Some(k)) => parts.push(Some(k)),
                            _ => {
                                ok = false;
                                break;
                            }
                        }
                    }
                    if ok {
                        pk_indexes.push((ix.name.clone(), Some(parts)));
                    }
                    continue;
                }
                _ => {}
            }
        }
        let ks = key::index_keys(cx, *clone, &ix.key_path, ix.multi_entry)?;
        index_keys.push((ix.name.clone(), ks));
    }
    Ok((key, index_keys, pk_indexes))
}

fn source_store_name(w: &Window, so: u32) -> String {
    with_store(w, so, |s| s.meta.name.clone()).unwrap_or_default()
}

/// `getAll`/`getAllKeys` arguments: a query or an options dictionary, and
/// a count.
fn get_all_args(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    op: &str,
    iface: &str,
) -> Result<(Range, Option<u32>, Dir), HostError> {
    // An options dictionary: a plain object that is not a key or range.
    if cx.len() > 0
        && cx.arg_kind(0) == ValueKind::Object
        && key_range_of(w, cx.arg_host(0)).is_none()
    {
        let h = cx.arg_handle(0);
        // Dates, arrays and binary are keys (valid or not), never options.
        let key_like = cx.handle_date(h).is_some()
            || cx.handle_is_array(h)
            || cx.is_array_buffer(h)
            || cx.handle_view_kind(h).is_some();
        if !key_like {
            let q = cx.get_property(h, "query")?;
            let range = if matches!(cx.handle_kind(q), ValueKind::Undefined | ValueKind::Null) {
                Range::all()
            } else if let Some(r) = key_range_of(w, cx.handle_host(q)) {
                r
            } else {
                match key::to_key(cx, q)? {
                    Some(k) => Range::only(k),
                    None => {
                        return Err(exec_err(
                            w,
                            "DataError",
                            op,
                            iface,
                            "The parameter is not a valid key.",
                        ));
                    }
                }
            };
            cx.release(q);
            let c = cx.get_property(h, "count")?;
            let count = match cx.handle_value(c) {
                JsValue::Number(n) if n.is_finite() && n >= 0.0 => Some(n as u32),
                _ => None,
            };
            cx.release(c);
            let d = cx.get_property(h, "direction")?;
            let dir = if cx.handle_kind(d) == ValueKind::Undefined {
                Dir::Next
            } else {
                let s = cx.handle_string(d)?;
                Dir::parse(&s).ok_or_else(|| {
                    exec_err(
                        w,
                        "TypeError",
                        op,
                        iface,
                        &format!(
                            "Failed to read the 'direction' property from 'IDBGetAllOptions': The provided value '{s}' is not a valid enum value of type IDBCursorDirection."
                        ),
                    )
                })?
            };
            cx.release(d);
            cx.release(h);
            return Ok((range, count, dir));
        }
        cx.release(h);
    }
    let range = range_arg(cx, w, 0, true, op, iface)?;
    let count = enforce_u32(cx, w, 1, op, iface)?;
    Ok((range, count, Dir::Next))
}

fn dir_arg(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    i: usize,
    op: &str,
    iface: &str,
) -> Result<Dir, HostError> {
    if cx.len() <= i || cx.arg_kind(i) == ValueKind::Undefined {
        return Ok(Dir::Next);
    }
    let s = cx.arg_string(i)?;
    Dir::parse(&s).ok_or_else(|| {
        exec_err(
            w,
            "TypeError",
            op,
            iface,
            &format!(
                "The provided value '{s}' is not a valid enum value of type IDBCursorDirection."
            ),
        )
    })
}

/// A cursor request.
fn open_cursor(
    w: &Rc<Window>,
    tx: u32,
    store: u32,
    index: Option<u32>,
    range: Range,
    dir: Dir,
    key_only: bool,
) -> Result<u32, HostError> {
    let req = new_idb(
        w,
        IdbObj::Request(Box::new(Req {
            open: false,
            done: false,
            result: ResultVal::Undefined,
            error: None,
            source: match index {
                Some(i) => Source::Index(i),
                None => Source::Store(store),
            },
            tx: Some(tx),
            cursor: None,
        })),
    );
    let cur = new_idb(
        w,
        IdbObj::Cursor(Box::new(Cur {
            req,
            store,
            index,
            dir,
            key_only,
            range: range.clone(),
            pos: None,
            got_value: false,
            key: None,
            primary: None,
            value: None,
        })),
    );
    with_req(w, req, |r| r.cursor = Some(cur));
    let id = with_tx(w, tx, |t| {
        t.pending.push_back(req);
        t.id
    })
    .unwrap_or(0);
    let store_name = source_store_name(w, store);
    let index_name = index.and_then(|i| with_index(w, i, |x| x.meta.name.clone()));
    backend(w)?.request(
        id,
        req,
        Op::Cursor {
            store: store_name,
            index: index_name,
            range,
            dir,
            key_only,
            pos: None,
            step: Step::First,
        },
    );
    Ok(req)
}

fn register_store(reg: &mut Registry, rt: &Runtime) {
    let iface = "IDBObjectStore";
    getter(reg, rt, iface, "name", |cx, w| {
        let id = this_of(cx, w, is_store)?;
        cx.ret(with_store(w, id, |s| s.meta.name.clone()).unwrap_or_default())
    });
    setter(reg, rt, iface, "name", |cx, w| {
        let id = this_of(cx, w, is_store)?;
        let name = cx.arg_string(0)?;
        let fail = |n: &str, m: &str| {
            w.rt.err(
                n,
                format!("Failed to set the 'name' property on 'IDBObjectStore': {m}"),
            )
        };
        let (tx, old, deleted) =
            with_store(w, id, |s| (s.tx, s.meta.name.clone(), s.deleted)).ok_or_else(illegal)?;
        let (mode, state, db) = with_tx(w, tx, |t| (t.mode, t.state, t.db)).ok_or_else(illegal)?;
        if mode != Mode::VersionChange {
            return Err(fail(
                "InvalidStateError",
                "The database is not running a version change transaction.",
            ));
        }
        if deleted {
            return Err(fail(
                "InvalidStateError",
                "The object store has been deleted.",
            ));
        }
        if state != TxState::Active {
            return Err(fail(
                "TransactionInactiveError",
                "The transaction is not active.",
            ));
        }
        if name == old {
            return Ok(());
        }
        if with_db(w, db, |d| d.meta.stores.iter().any(|s| s.name == name)).unwrap_or(false) {
            return Err(fail(
                "ConstraintError",
                "An object store with the specified name already exists.",
            ));
        }
        let txn = with_tx(w, tx, |t| t.id).unwrap_or(0);
        let _ = backend(w)?.schema(txn, Schema::RenameStore(old.clone(), name.clone()));
        with_db(w, db, |d| {
            for s in &mut d.meta.stores {
                if s.name == old {
                    s.name = name.clone();
                }
            }
            d.meta
                .stores
                .sort_by(|a, b| a.name.encode_utf16().cmp(b.name.encode_utf16()));
        });
        with_tx(w, tx, |t| {
            for s in &mut t.scope {
                if *s == old {
                    *s = name.clone();
                }
            }
            t.scope.sort();
            if let Some(o) = t.stores.remove(&old) {
                t.stores.insert(name.clone(), o);
            }
        });
        with_store(w, id, |s| s.meta.name = name);
        Ok(())
    });
    getter(reg, rt, iface, "keyPath", |cx, w| {
        let id = this_of(cx, w, is_store)?;
        let (cached, kp) =
            with_store(w, id, |s| (s.key_path, s.meta.key_path.clone())).ok_or_else(illegal)?;
        if let Some(h) = cached {
            return cx.ret(Arg::Handle(h));
        }
        let Some(kp) = kp else {
            return cx.ret(Arg::Null);
        };
        let h = cx.make_handle(kp.to_arg())?;
        with_store(w, id, |s| s.key_path = Some(h));
        cx.ret(Arg::Handle(h))
    });
    getter(reg, rt, iface, "indexNames", |cx, w| {
        let id = this_of(cx, w, is_store)?;
        let names = store_indexes(w, id).into_iter().map(|i| i.name).collect();
        cx.ret(list_arg(w, names))
    });
    getter(reg, rt, iface, "transaction", |cx, w| {
        let id = this_of(cx, w, is_store)?;
        let tx = with_store(w, id, |s| s.tx).ok_or_else(illegal)?;
        cx.ret(w.obj_arg(tx))
    });
    getter(reg, rt, iface, "autoIncrement", |cx, w| {
        let id = this_of(cx, w, is_store)?;
        cx.ret(with_store(w, id, |s| s.meta.auto_increment).unwrap_or(false))
    });
    method(reg, rt, iface, "put", |cx, w| {
        let id = this_of(cx, w, is_store)?;
        let r = put_impl(cx, w, id, "put", false)?;
        cx.ret(w.obj_arg(r))
    });
    method(reg, rt, iface, "add", |cx, w| {
        let id = this_of(cx, w, is_store)?;
        let r = put_impl(cx, w, id, "add", true)?;
        cx.ret(w.obj_arg(r))
    });
    method(reg, rt, iface, "delete", |cx, w| {
        let id = this_of(cx, w, is_store)?;
        need_args(cx, 1, "delete", "IDBObjectStore")?;
        let (tx, meta) = store_checks(w, id, "delete", true)?;
        let range = range_arg(cx, w, 0, false, "delete", "IDBObjectStore")?;
        let r = place(
            w,
            tx,
            Source::Store(id),
            Op::Delete {
                store: meta.name,
                range,
            },
        )?;
        cx.ret(w.obj_arg(r))
    });
    method(reg, rt, iface, "clear", |cx, w| {
        let id = this_of(cx, w, is_store)?;
        let (tx, meta) = store_checks(w, id, "clear", true)?;
        let r = place(w, tx, Source::Store(id), Op::Clear { store: meta.name })?;
        cx.ret(w.obj_arg(r))
    });
    for (name, key_only) in [("get", false), ("getKey", true)] {
        method(reg, rt, iface, name, move |cx, w| {
            let id = this_of(cx, w, is_store)?;
            need_args(cx, 1, name, "IDBObjectStore")?;
            let (tx, meta) = store_checks(w, id, name, false)?;
            let range = range_arg(cx, w, 0, false, name, "IDBObjectStore")?;
            let r = place(
                w,
                tx,
                Source::Store(id),
                Op::Get {
                    store: meta.name,
                    index: None,
                    range,
                    key_only,
                },
            )?;
            cx.ret(w.obj_arg(r))
        });
    }
    for (name, kind) in [
        ("getAll", AllKind::Values),
        ("getAllKeys", AllKind::Keys),
        ("getAllRecords", AllKind::Records),
    ] {
        method(reg, rt, iface, name, move |cx, w| {
            let id = this_of(cx, w, is_store)?;
            let (tx, meta) = store_checks(w, id, name, false)?;
            let (range, count, dir) = get_all_args(cx, w, name, "IDBObjectStore")?;
            let r = place(
                w,
                tx,
                Source::Store(id),
                Op::GetAll {
                    store: meta.name,
                    index: None,
                    range,
                    count,
                    dir,
                    kind,
                },
            )?;
            cx.ret(w.obj_arg(r))
        });
    }
    method(reg, rt, iface, "count", |cx, w| {
        let id = this_of(cx, w, is_store)?;
        let (tx, meta) = store_checks(w, id, "count", false)?;
        let range = range_arg(cx, w, 0, true, "count", "IDBObjectStore")?;
        let r = place(
            w,
            tx,
            Source::Store(id),
            Op::Count {
                store: meta.name,
                index: None,
                range,
            },
        )?;
        cx.ret(w.obj_arg(r))
    });
    for (name, key_only) in [("openCursor", false), ("openKeyCursor", true)] {
        method(reg, rt, iface, name, move |cx, w| {
            let id = this_of(cx, w, is_store)?;
            let (tx, _) = store_checks(w, id, name, false)?;
            let range = range_arg(cx, w, 0, true, name, "IDBObjectStore")?;
            let dir = dir_arg(cx, w, 1, name, "IDBObjectStore")?;
            let r = open_cursor(w, tx, id, None, range, dir, key_only)?;
            cx.ret(w.obj_arg(r))
        });
    }
    method(reg, rt, iface, "index", |cx, w| {
        let id = this_of(cx, w, is_store)?;
        need_args(cx, 1, "index", "IDBObjectStore")?;
        let name = cx.arg_string(0)?;
        let (tx, deleted) = with_store(w, id, |s| (s.tx, s.deleted)).ok_or_else(illegal)?;
        if deleted {
            return Err(exec_err(
                w,
                "InvalidStateError",
                "index",
                "IDBObjectStore",
                "The object store has been deleted.",
            ));
        }
        if matches!(tx_state(w, tx), TxState::Finished) {
            return Err(exec_err(
                w,
                "InvalidStateError",
                "index",
                "IDBObjectStore",
                "The transaction has finished.",
            ));
        }
        if let Some(i) = with_store(w, id, |s| s.indexes.get(&name).copied()).flatten() {
            return cx.ret(w.obj_arg(i));
        }
        let Some(meta) = store_indexes(w, id).into_iter().find(|i| i.name == name) else {
            return Err(exec_err(
                w,
                "NotFoundError",
                "index",
                "IDBObjectStore",
                "The specified index was not found.",
            ));
        };
        let i = index_obj(w, id, meta);
        cx.ret(w.obj_arg(i))
    });
    method(reg, rt, iface, "createIndex", |cx, w| {
        let id = this_of(cx, w, is_store)?;
        need_args(cx, 2, "createIndex", "IDBObjectStore")?;
        let (op, iface) = ("createIndex", "IDBObjectStore");
        let name = cx.arg_string(0)?;
        let kph = cx.arg_handle(1);
        let kp = key::key_path_arg(cx, kph, false);
        cx.release(kph);
        let kp = kp?.unwrap_or(KeyPath::String(String::new()));
        let (mut unique, mut multi) = (false, false);
        if cx.len() > 2 && cx.arg_kind(2) == ValueKind::Object {
            let h = cx.arg_handle(2);
            for (k, slot) in [("unique", &mut unique), ("multiEntry", &mut multi)] {
                let v = cx.get_property(h, k)?;
                *slot = !matches!(
                    cx.handle_value(v),
                    JsValue::Undefined | JsValue::Null | JsValue::Bool(false)
                ) && !matches!(cx.handle_value(v), JsValue::Number(n) if n == 0.0 || n.is_nan())
                    && !matches!(cx.handle_value(v), JsValue::String(ref s) if s.is_empty());
                cx.release(v);
            }
            cx.release(h);
        }
        let (tx, deleted, store_name) =
            with_store(w, id, |s| (s.tx, s.deleted, s.meta.name.clone())).ok_or_else(illegal)?;
        let mode = with_tx(w, tx, |t| t.mode).unwrap_or(Mode::ReadOnly);
        if mode != Mode::VersionChange {
            return Err(exec_err(
                w,
                "InvalidStateError",
                op,
                iface,
                "The database is not running a version change transaction.",
            ));
        }
        if deleted {
            return Err(exec_err(
                w,
                "InvalidStateError",
                op,
                iface,
                "The object store has been deleted.",
            ));
        }
        check_active(w, tx, op, iface)?;
        if store_indexes(w, id).iter().any(|i| i.name == name) {
            return Err(exec_err(
                w,
                "ConstraintError",
                op,
                iface,
                "An index with the specified name already exists.",
            ));
        }
        if !kp.is_valid() {
            return Err(exec_err(
                w,
                "SyntaxError",
                op,
                iface,
                "The keyPath argument contains an invalid key path.",
            ));
        }
        if kp.is_array() && multi {
            return Err(exec_err(
                w,
                "InvalidAccessError",
                op,
                iface,
                "The keyPath argument was an array and the multiEntry option is true.",
            ));
        }
        let meta = IndexMeta {
            name: name.clone(),
            key_path: kp,
            unique,
            multi_entry: multi,
        };
        // Index keys of the existing records, from their clones.
        let txn = with_tx(w, tx, |t| t.id).unwrap_or(0);
        let b = backend(w)?;
        let auto_path = with_store(w, id, |s| match (&s.meta.key_path, s.meta.auto_increment) {
            (Some(KeyPath::String(p)), true) => Some(p.clone()),
            _ => None,
        })
        .flatten();
        let mut keys = Vec::new();
        for (pk, v) in b.store_values(txn, &store_name) {
            let h = clone_in_realm(cx, w, &v)?;
            if let Some(p) = &auto_path {
                key::inject(cx, h, p, &pk);
            }
            let ks = key::index_keys(cx, h, &meta.key_path, meta.multi_entry);
            cx.release(h);
            keys.push((pk, ks?));
        }
        let built = b.schema(
            txn,
            Schema::CreateIndex(store_name.clone(), meta.clone(), keys),
        );
        let db = with_tx(w, tx, |t| t.db).unwrap_or(0);
        with_db(w, db, |d| {
            if let Some(s) = d.meta.stores.iter_mut().find(|s| s.name == store_name) {
                s.indexes.push(meta.clone());
                s.indexes
                    .sort_by(|a, b| a.name.encode_utf16().cmp(b.name.encode_utf16()));
            }
        });
        let i = index_obj(w, id, meta);
        if let Err(msg) = built {
            // Chrome aborts the upgrade (no request event) instead of
            // committing it.
            let e = dom_exception(w, "ConstraintError", &msg);
            with_tx(w, tx, |t| t.doomed = Some(e.clone()));
            let _ = w.inbox().send(Box::new(move |_js, w| {
                if let Some(e) = with_tx(w, tx, |t| t.doomed.take()).flatten() {
                    abort_tx(w, tx, Some(e));
                }
            }));
        }
        cx.ret(w.obj_arg(i))
    });
    method(reg, rt, iface, "deleteIndex", |cx, w| {
        let id = this_of(cx, w, is_store)?;
        need_args(cx, 1, "deleteIndex", "IDBObjectStore")?;
        let (op, iface) = ("deleteIndex", "IDBObjectStore");
        let name = cx.arg_string(0)?;
        let (tx, deleted, store_name) =
            with_store(w, id, |s| (s.tx, s.deleted, s.meta.name.clone())).ok_or_else(illegal)?;
        if with_tx(w, tx, |t| t.mode).unwrap_or(Mode::ReadOnly) != Mode::VersionChange {
            return Err(exec_err(
                w,
                "InvalidStateError",
                op,
                iface,
                "The database is not running a version change transaction.",
            ));
        }
        if deleted {
            return Err(exec_err(
                w,
                "InvalidStateError",
                op,
                iface,
                "The object store has been deleted.",
            ));
        }
        check_active(w, tx, op, iface)?;
        if !store_indexes(w, id).iter().any(|i| i.name == name) {
            return Err(exec_err(
                w,
                "NotFoundError",
                op,
                iface,
                "The specified index was not found.",
            ));
        }
        let txn = with_tx(w, tx, |t| t.id).unwrap_or(0);
        let _ = backend(w)?.schema(txn, Schema::DeleteIndex(store_name.clone(), name.clone()));
        let db = with_tx(w, tx, |t| t.db).unwrap_or(0);
        with_db(w, db, |d| {
            if let Some(s) = d.meta.stores.iter_mut().find(|s| s.name == store_name) {
                s.indexes.retain(|i| i.name != name);
            }
        });
        if let Some(i) = with_store(w, id, |s| s.indexes.remove(&name)).flatten() {
            with_index(w, i, |x| x.deleted = true);
        }
        cx.ret(Arg::Undefined)
    });
}

fn index_obj(w: &Window, store: u32, meta: IndexMeta) -> u32 {
    let name = meta.name.clone();
    let (tx, sorig) = with_store(w, store, |s| (s.tx, s.orig.clone())).unwrap_or((0, None));
    let orig = with_tx(w, tx, |t| match &t.old_meta {
        Some(old) => sorig.as_ref().and_then(|sn| {
            old.stores
                .iter()
                .find(|s| s.name == *sn)
                .filter(|s| s.indexes.iter().any(|i| i.name == name))
                .map(|_| name.clone())
        }),
        None => Some(name.clone()),
    })
    .flatten();
    let i = new_idb(
        w,
        IdbObj::Index(Box::new(IndexObj {
            store,
            orig,
            meta,
            deleted: false,
            key_path: None,
        })),
    );
    with_store(w, store, |s| {
        s.indexes.insert(name, i);
        s.all_indexes.push(i);
    });
    i
}

fn is_index(o: &IdbObj) -> bool {
    matches!(o, IdbObj::Index(_))
}

/// Checks of index request methods.
fn index_checks(w: &Window, ix: u32, op: &str) -> Result<(u32, u32, String, String), HostError> {
    let (store, deleted, name) =
        with_index(w, ix, |x| (x.store, x.deleted, x.meta.name.clone())).ok_or_else(illegal)?;
    let (tx, sdel, sname) =
        with_store(w, store, |s| (s.tx, s.deleted, s.meta.name.clone())).ok_or_else(illegal)?;
    if deleted || sdel {
        return Err(exec_err(
            w,
            "InvalidStateError",
            op,
            "IDBIndex",
            "The index or its object store has been deleted.",
        ));
    }
    check_active(w, tx, op, "IDBIndex")?;
    Ok((tx, store, sname, name))
}

fn register_index(reg: &mut Registry, rt: &Runtime) {
    let iface = "IDBIndex";
    getter(reg, rt, iface, "name", |cx, w| {
        let id = this_of(cx, w, is_index)?;
        cx.ret(with_index(w, id, |x| x.meta.name.clone()).unwrap_or_default())
    });
    setter(reg, rt, iface, "name", |cx, w| {
        let id = this_of(cx, w, is_index)?;
        let name = cx.arg_string(0)?;
        let fail = |n: &str, m: &str| {
            w.rt.err(
                n,
                format!("Failed to set the 'name' property on 'IDBIndex': {m}"),
            )
        };
        let (store, old, deleted) =
            with_index(w, id, |x| (x.store, x.meta.name.clone(), x.deleted)).ok_or_else(illegal)?;
        let (tx, sname) =
            with_store(w, store, |s| (s.tx, s.meta.name.clone())).ok_or_else(illegal)?;
        let (mode, state, db) = with_tx(w, tx, |t| (t.mode, t.state, t.db)).ok_or_else(illegal)?;
        if mode != Mode::VersionChange {
            return Err(fail(
                "InvalidStateError",
                "The database is not running a version change transaction.",
            ));
        }
        if deleted {
            return Err(fail(
                "InvalidStateError",
                "The index or its object store has been deleted.",
            ));
        }
        if state != TxState::Active {
            return Err(fail(
                "TransactionInactiveError",
                "The transaction is not active.",
            ));
        }
        if name == old {
            return Ok(());
        }
        if store_indexes(w, store).iter().any(|i| i.name == name) {
            return Err(fail(
                "ConstraintError",
                "An index with the specified name already exists.",
            ));
        }
        let txn = with_tx(w, tx, |t| t.id).unwrap_or(0);
        let _ = backend(w)?.schema(
            txn,
            Schema::RenameIndex(sname.clone(), old.clone(), name.clone()),
        );
        with_db(w, db, |d| {
            if let Some(s) = d.meta.stores.iter_mut().find(|s| s.name == sname) {
                for i in &mut s.indexes {
                    if i.name == old {
                        i.name = name.clone();
                    }
                }
                s.indexes
                    .sort_by(|a, b| a.name.encode_utf16().cmp(b.name.encode_utf16()));
            }
        });
        with_store(w, store, |s| {
            if let Some(o) = s.indexes.remove(&old) {
                s.indexes.insert(name.clone(), o);
            }
        });
        with_index(w, id, |x| x.meta.name = name);
        Ok(())
    });
    getter(reg, rt, iface, "objectStore", |cx, w| {
        let id = this_of(cx, w, is_index)?;
        let s = with_index(w, id, |x| x.store).ok_or_else(illegal)?;
        cx.ret(w.obj_arg(s))
    });
    getter(reg, rt, iface, "keyPath", |cx, w| {
        let id = this_of(cx, w, is_index)?;
        let (cached, kp) =
            with_index(w, id, |x| (x.key_path, x.meta.key_path.clone())).ok_or_else(illegal)?;
        if let Some(h) = cached {
            return cx.ret(Arg::Handle(h));
        }
        let h = cx.make_handle(kp.to_arg())?;
        with_index(w, id, |x| x.key_path = Some(h));
        cx.ret(Arg::Handle(h))
    });
    getter(reg, rt, iface, "multiEntry", |cx, w| {
        let id = this_of(cx, w, is_index)?;
        cx.ret(with_index(w, id, |x| x.meta.multi_entry).unwrap_or(false))
    });
    getter(reg, rt, iface, "unique", |cx, w| {
        let id = this_of(cx, w, is_index)?;
        cx.ret(with_index(w, id, |x| x.meta.unique).unwrap_or(false))
    });
    for (name, key_only) in [("get", false), ("getKey", true)] {
        method(reg, rt, iface, name, move |cx, w| {
            let id = this_of(cx, w, is_index)?;
            need_args(cx, 1, name, "IDBIndex")?;
            let (tx, store, sname, iname) = index_checks(w, id, name)?;
            let range = range_arg(cx, w, 0, false, name, "IDBIndex")?;
            let _ = store;
            let r = place(
                w,
                tx,
                Source::Index(id),
                Op::Get {
                    store: sname,
                    index: Some(iname),
                    range,
                    key_only,
                },
            )?;
            cx.ret(w.obj_arg(r))
        });
    }
    for (name, kind) in [
        ("getAll", AllKind::Values),
        ("getAllKeys", AllKind::Keys),
        ("getAllRecords", AllKind::Records),
    ] {
        method(reg, rt, iface, name, move |cx, w| {
            let id = this_of(cx, w, is_index)?;
            let (tx, _, sname, iname) = index_checks(w, id, name)?;
            let (range, count, dir) = get_all_args(cx, w, name, "IDBIndex")?;
            let r = place(
                w,
                tx,
                Source::Index(id),
                Op::GetAll {
                    store: sname,
                    index: Some(iname),
                    range,
                    count,
                    dir,
                    kind,
                },
            )?;
            cx.ret(w.obj_arg(r))
        });
    }
    method(reg, rt, iface, "count", |cx, w| {
        let id = this_of(cx, w, is_index)?;
        let (tx, _, sname, iname) = index_checks(w, id, "count")?;
        let range = range_arg(cx, w, 0, true, "count", "IDBIndex")?;
        let r = place(
            w,
            tx,
            Source::Index(id),
            Op::Count {
                store: sname,
                index: Some(iname),
                range,
            },
        )?;
        cx.ret(w.obj_arg(r))
    });
    for (name, key_only) in [("openCursor", false), ("openKeyCursor", true)] {
        method(reg, rt, iface, name, move |cx, w| {
            let id = this_of(cx, w, is_index)?;
            let (tx, store, _, _) = index_checks(w, id, name)?;
            let range = range_arg(cx, w, 0, true, name, "IDBIndex")?;
            let dir = dir_arg(cx, w, 1, name, "IDBIndex")?;
            let r = open_cursor(w, tx, store, Some(id), range, dir, key_only)?;
            cx.ret(w.obj_arg(r))
        });
    }
}

fn is_cursor(o: &IdbObj) -> bool {
    matches!(o, IdbObj::Cursor(_))
}

/// Cursor checks shared by `continue`, `advance`, `continuePrimaryKey`,
/// `update` and `delete`: transaction active, source not deleted.
fn cursor_checks(w: &Window, cur: u32, op: &str) -> Result<(u32, u32, Option<u32>), HostError> {
    let (store, index) = with_cur(w, cur, |c| (c.store, c.index)).ok_or_else(illegal)?;
    let tx = with_store(w, store, |s| s.tx).ok_or_else(illegal)?;
    check_active(w, tx, op, "IDBCursor")?;
    let deleted = with_store(w, store, |s| s.deleted).unwrap_or(true)
        || index.is_some_and(|i| with_index(w, i, |x| x.deleted).unwrap_or(true));
    if deleted {
        return Err(exec_err(
            w,
            "InvalidStateError",
            op,
            "IDBCursor",
            "The cursor's source or effective object store has been deleted.",
        ));
    }
    Ok((tx, store, index))
}

fn iterated_err(w: &Window, op: &str) -> HostError {
    exec_err(
        w,
        "InvalidStateError",
        op,
        "IDBCursor",
        "The cursor is being iterated or has iterated past its end.",
    )
}

/// Move a cursor (`continue`, `advance`, `continuePrimaryKey`).
fn step_cursor(w: &Rc<Window>, cur: u32, tx: u32, step: Step) -> Result<(), HostError> {
    let (req, store, index, dir, key_only, range, pos) = with_cur(w, cur, |c| {
        c.got_value = false;
        (
            c.req,
            c.store,
            c.index,
            c.dir,
            c.key_only,
            c.range.clone(),
            c.pos.clone(),
        )
    })
    .ok_or_else(illegal)?;
    let store_name = source_store_name(w, store);
    let index_name = index.and_then(|i| with_index(w, i, |x| x.meta.name.clone()));
    replace_cursor(
        w,
        tx,
        req,
        Op::Cursor {
            store: store_name,
            index: index_name,
            range,
            dir,
            key_only,
            pos,
            step,
        },
    )
}

fn register_cursor(reg: &mut Registry, rt: &Runtime) {
    let iface = "IDBCursor";
    getter(reg, rt, iface, "source", |cx, w| {
        let id = this_of(cx, w, is_cursor)?;
        let (store, index) = with_cur(w, id, |c| (c.store, c.index)).ok_or_else(illegal)?;
        cx.ret(w.obj_arg(index.unwrap_or(store)))
    });
    getter(reg, rt, iface, "direction", |cx, w| {
        let id = this_of(cx, w, is_cursor)?;
        cx.ret(with_cur(w, id, |c| c.dir.as_str()).unwrap_or("next"))
    });
    getter(reg, rt, iface, "key", |cx, w| {
        let id = this_of(cx, w, is_cursor)?;
        cx.ret(
            with_cur(w, id, |c| c.key)
                .flatten()
                .map_or(Arg::Undefined, Arg::Handle),
        )
    });
    getter(reg, rt, iface, "primaryKey", |cx, w| {
        let id = this_of(cx, w, is_cursor)?;
        cx.ret(
            with_cur(w, id, |c| c.primary)
                .flatten()
                .map_or(Arg::Undefined, Arg::Handle),
        )
    });
    getter(reg, rt, iface, "request", |cx, w| {
        let id = this_of(cx, w, is_cursor)?;
        let r = with_cur(w, id, |c| c.req).ok_or_else(illegal)?;
        cx.ret(w.obj_arg(r))
    });
    getter(reg, rt, "IDBCursorWithValue", "value", |cx, w| {
        let id = this_of(cx, w, is_cursor)?;
        cx.ret(
            with_cur(w, id, |c| c.value)
                .flatten()
                .map_or(Arg::Undefined, Arg::Handle),
        )
    });
    method(reg, rt, iface, "advance", |cx, w| {
        let id = this_of(cx, w, is_cursor)?;
        need_args(cx, 1, "advance", "IDBCursor")?;
        let n = enforce_u32(cx, w, 0, "advance", "IDBCursor")?.unwrap_or(0);
        if n == 0 {
            return Err(exec_err(
                w,
                "TypeError",
                "advance",
                "IDBCursor",
                "A count argument with value 0 (zero) was supplied, must be greater than 0.",
            ));
        }
        let (tx, _, _) = cursor_checks(w, id, "advance")?;
        if !with_cur(w, id, |c| c.got_value).unwrap_or(false) {
            return Err(iterated_err(w, "advance"));
        }
        step_cursor(w, id, tx, Step::Count(n))?;
        cx.ret(Arg::Undefined)
    });
    method(reg, rt, iface, "continue", |cx, w| {
        let id = this_of(cx, w, is_cursor)?;
        let (tx, _, _) = cursor_checks(w, id, "continue")?;
        if !with_cur(w, id, |c| c.got_value).unwrap_or(false) {
            return Err(iterated_err(w, "continue"));
        }
        let step =
            if cx.len() > 0 && !matches!(cx.arg_kind(0), ValueKind::Undefined | ValueKind::Null) {
                let k = key_arg(cx, w, 0, "continue", "IDBCursor")?;
                let (pos, dir) = with_cur(w, id, |c| (c.pos.clone(), c.dir)).ok_or_else(illegal)?;
                if let Some((cur_key, _)) = pos {
                    let bad = match dir {
                        Dir::Next | Dir::NextUnique => k <= cur_key,
                        Dir::Prev | Dir::PrevUnique => k >= cur_key,
                    };
                    if bad {
                        let msg = if matches!(dir, Dir::Next | Dir::NextUnique) {
                            "The parameter is less than or equal to this cursor's position."
                        } else {
                            "The parameter is greater than or equal to this cursor's position."
                        };
                        return Err(exec_err(w, "DataError", "continue", "IDBCursor", msg));
                    }
                }
                Step::Key(k)
            } else {
                Step::Count(1)
            };
        step_cursor(w, id, tx, step)?;
        cx.ret(Arg::Undefined)
    });
    method(reg, rt, iface, "continuePrimaryKey", |cx, w| {
        let id = this_of(cx, w, is_cursor)?;
        need_args(cx, 2, "continuePrimaryKey", "IDBCursor")?;
        let op = "continuePrimaryKey";
        let (tx, _, index) = cursor_checks(w, id, op)?;
        if index.is_none() {
            return Err(exec_err(
                w,
                "InvalidAccessError",
                op,
                "IDBCursor",
                "The cursor's source is not an index.",
            ));
        }
        let dir = with_cur(w, id, |c| c.dir).ok_or_else(illegal)?;
        if !matches!(dir, Dir::Next | Dir::Prev) {
            return Err(exec_err(
                w,
                "InvalidAccessError",
                op,
                "IDBCursor",
                "The cursor's direction is not 'next' or 'prev'.",
            ));
        }
        if !with_cur(w, id, |c| c.got_value).unwrap_or(false) {
            return Err(iterated_err(w, op));
        }
        let k = key_arg(cx, w, 0, op, "IDBCursor")?;
        let pk = key_arg(cx, w, 1, op, "IDBCursor")?;
        let pos = with_cur(w, id, |c| c.pos.clone()).flatten();
        if let Some((ck, cpk)) = pos {
            let fwd = dir == Dir::Next;
            let key_bad = if fwd { k < ck } else { k > ck };
            if key_bad {
                return Err(exec_err(
                    w,
                    "DataError",
                    op,
                    "IDBCursor",
                    if fwd {
                        "The parameter is less than this cursor's position."
                    } else {
                        "The parameter is greater than this cursor's position."
                    },
                ));
            }
            let pk_bad = k == ck && if fwd { pk <= cpk } else { pk >= cpk };
            if pk_bad {
                return Err(exec_err(
                    w,
                    "DataError",
                    op,
                    "IDBCursor",
                    if fwd {
                        "The parameter is less than or equal to this cursor's position."
                    } else {
                        "The parameter is greater than or equal to this cursor's position."
                    },
                ));
            }
        }
        step_cursor(w, id, tx, Step::KeyPrimary(k, pk))?;
        cx.ret(Arg::Undefined)
    });
    method(reg, rt, iface, "update", |cx, w| {
        let id = this_of(cx, w, is_cursor)?;
        need_args(cx, 1, "update", "IDBCursor")?;
        let op = "update";
        let (tx, store, _) = cursor_checks(w, id, op)?;
        if with_tx(w, tx, |t| t.mode == Mode::ReadOnly).unwrap_or(false) {
            return Err(exec_err(
                w,
                "ReadOnlyError",
                op,
                "IDBCursor",
                "The record may not be updated inside a read-only transaction.",
            ));
        }
        let (got, key_only, pos) =
            with_cur(w, id, |c| (c.got_value, c.key_only, c.pos.clone())).ok_or_else(illegal)?;
        if !got {
            return Err(iterated_err(w, op));
        }
        if key_only {
            return Err(exec_err(
                w,
                "InvalidStateError",
                op,
                "IDBCursor",
                "The cursor is a key cursor.",
            ));
        }
        let pk = pos.map(|p| p.1).ok_or_else(illegal)?;
        let meta = with_store(w, store, |s| s.meta.clone()).ok_or_else(illegal)?;
        let vh = cx.arg_handle(0);
        let value = inactive_during(w, tx, || clone_value(cx, w, vh, op, "IDBCursor"));
        cx.release(vh);
        let value = value?;
        let clone = clone_in_realm(cx, w, &value)?;
        if let Some(kp) = &meta.key_path {
            let k = key::eval_key(cx, clone, kp)?;
            if !matches!(&k, Ok(Some(k)) if *k == pk) {
                cx.release(clone);
                return Err(exec_err(
                    w,
                    "DataError",
                    op,
                    "IDBCursor",
                    "The effective object store of this cursor uses in-line keys and evaluating the key path of the value parameter results in a different value than the cursor's effective key.",
                ));
            }
        }
        let mut index_keys = Vec::new();
        for ix in store_indexes(w, store) {
            let ks = key::index_keys(cx, clone, &ix.key_path, ix.multi_entry);
            index_keys.push((ix.name.clone(), ks?));
        }
        cx.release(clone);
        let r = place(
            w,
            tx,
            Source::Cursor(id),
            Op::Put {
                store: meta.name,
                value,
                key: Some(pk),
                add: false,
                index_keys,
                pk_indexes: Vec::new(),
            },
        )?;
        cx.ret(w.obj_arg(r))
    });
    method(reg, rt, iface, "delete", |cx, w| {
        let id = this_of(cx, w, is_cursor)?;
        let op = "delete";
        let (tx, store, _) = cursor_checks(w, id, op)?;
        if with_tx(w, tx, |t| t.mode == Mode::ReadOnly).unwrap_or(false) {
            return Err(exec_err(
                w,
                "ReadOnlyError",
                op,
                "IDBCursor",
                "The record may not be deleted inside a read-only transaction.",
            ));
        }
        let (got, key_only, pos) =
            with_cur(w, id, |c| (c.got_value, c.key_only, c.pos.clone())).ok_or_else(illegal)?;
        if !got {
            return Err(iterated_err(w, op));
        }
        if key_only {
            return Err(exec_err(
                w,
                "InvalidStateError",
                op,
                "IDBCursor",
                "The cursor is a key cursor.",
            ));
        }
        let pk = pos.map(|p| p.1).ok_or_else(illegal)?;
        let name = source_store_name(w, store);
        let r = place(
            w,
            tx,
            Source::Cursor(id),
            Op::Delete {
                store: name,
                range: Range::only(pk),
            },
        )?;
        cx.ret(w.obj_arg(r))
    });
}

fn is_range(o: &IdbObj) -> bool {
    matches!(o, IdbObj::KeyRange(_))
}

fn this_range(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<Range, HostError> {
    let id = this_of(cx, w, is_range)?;
    with(w, id, |o| match o {
        IdbObj::KeyRange(r) => Some(r.clone()),
        _ => None,
    })
    .flatten()
    .ok_or_else(illegal)
}

fn range_obj(w: &Window, r: Range) -> Arg {
    w.obj_arg(new_idb(w, IdbObj::KeyRange(r)))
}

fn bool_arg(cx: &mut CallCtx<'_, '_, '_>, i: usize) -> Result<bool, HostError> {
    if cx.len() <= i {
        return Ok(false);
    }
    Ok(cx.arg_bool(i))
}

fn register_key_range(reg: &mut Registry, rt: &Runtime) {
    let iface = "IDBKeyRange";
    getter(reg, rt, iface, "lower", |cx, w| {
        let r = this_range(cx, w)?;
        cx.ret(r.lower.map_or(Arg::Undefined, |k| k.to_arg()))
    });
    getter(reg, rt, iface, "upper", |cx, w| {
        let r = this_range(cx, w)?;
        cx.ret(r.upper.map_or(Arg::Undefined, |k| k.to_arg()))
    });
    getter(reg, rt, iface, "lowerOpen", |cx, w| {
        let r = this_range(cx, w)?;
        cx.ret(r.lower_open)
    });
    getter(reg, rt, iface, "upperOpen", |cx, w| {
        let r = this_range(cx, w)?;
        cx.ret(r.upper_open)
    });
    method(reg, rt, iface, "includes", |cx, w| {
        let r = this_range(cx, w)?;
        need_args(cx, 1, "includes", "IDBKeyRange")?;
        let k = key_arg(cx, w, 0, "includes", "IDBKeyRange")?;
        cx.ret(r.contains(&k))
    });
    reg.register(
        iface,
        "only",
        ImplKind::StaticMethod,
        super::wf(rt, |cx, w| {
            need_args(cx, 1, "only", "IDBKeyRange")?;
            let k = key_arg(cx, w, 0, "only", "IDBKeyRange")?;
            cx.ret(range_obj(w, Range::only(k)))
        }),
    );
    reg.register(
        iface,
        "lowerBound",
        ImplKind::StaticMethod,
        super::wf(rt, |cx, w| {
            need_args(cx, 1, "lowerBound", "IDBKeyRange")?;
            let k = key_arg(cx, w, 0, "lowerBound", "IDBKeyRange")?;
            let open = bool_arg(cx, 1)?;
            cx.ret(range_obj(
                w,
                Range {
                    lower: Some(k),
                    upper: None,
                    lower_open: open,
                    upper_open: true,
                },
            ))
        }),
    );
    reg.register(
        iface,
        "upperBound",
        ImplKind::StaticMethod,
        super::wf(rt, |cx, w| {
            need_args(cx, 1, "upperBound", "IDBKeyRange")?;
            let k = key_arg(cx, w, 0, "upperBound", "IDBKeyRange")?;
            let open = bool_arg(cx, 1)?;
            cx.ret(range_obj(
                w,
                Range {
                    lower: None,
                    upper: Some(k),
                    lower_open: true,
                    upper_open: open,
                },
            ))
        }),
    );
    reg.register(
        iface,
        "bound",
        ImplKind::StaticMethod,
        super::wf(rt, |cx, w| {
            need_args(cx, 2, "bound", "IDBKeyRange")?;
            let lo = key_arg(cx, w, 0, "bound", "IDBKeyRange")?;
            let hi = key_arg(cx, w, 1, "bound", "IDBKeyRange")?;
            let lo_open = bool_arg(cx, 2)?;
            let hi_open = bool_arg(cx, 3)?;
            match lo.cmp(&hi) {
                std::cmp::Ordering::Greater => {
                    return Err(exec_err(
                        w,
                        "DataError",
                        "bound",
                        "IDBKeyRange",
                        "The lower key is greater than the upper key.",
                    ));
                }
                std::cmp::Ordering::Equal if lo_open || hi_open => {
                    return Err(exec_err(
                        w,
                        "DataError",
                        "bound",
                        "IDBKeyRange",
                        "The lower key and upper key are equal and one of the bounds is open.",
                    ));
                }
                _ => {}
            }
            cx.ret(range_obj(
                w,
                Range {
                    lower: Some(lo),
                    upper: Some(hi),
                    lower_open: lo_open,
                    upper_open: hi_open,
                },
            ))
        }),
    );
}

fn register_misc(reg: &mut Registry, rt: &Runtime) {
    // IDBRecord
    let rec = |o: &IdbObj| matches!(o, IdbObj::Record(_));
    for (name, pick) in [("key", 0u8), ("primaryKey", 1), ("value", 2)] {
        getter(reg, rt, "IDBRecord", name, move |cx, w| {
            let id = this_of(cx, w, rec)?;
            let h = with(w, id, |o| match o {
                IdbObj::Record(r) => Some(match pick {
                    0 => r.key,
                    1 => r.primary,
                    _ => r.value,
                }),
                _ => None,
            })
            .flatten()
            .ok_or_else(illegal)?;
            cx.ret(Arg::Handle(h))
        });
    }
    // DOMStringList
    let list = |o: &IdbObj| matches!(o, IdbObj::StringList(_));
    let items = |w: &Window, id: u32| -> Vec<String> {
        with(w, id, |o| match o {
            IdbObj::StringList(l) => l.clone(),
            _ => Vec::new(),
        })
        .unwrap_or_default()
    };
    getter(reg, rt, "DOMStringList", "length", move |cx, w| {
        let id = this_of(cx, w, list)?;
        cx.ret(items(w, id).len() as u32)
    });
    method(reg, rt, "DOMStringList", "item", move |cx, w| {
        let id = this_of(cx, w, list)?;
        need_args(cx, 1, "item", "DOMStringList")?;
        let i = cx.arg_u32(0)? as usize;
        match items(w, id).get(i) {
            Some(s) => cx.ret(s.clone()),
            None => cx.ret(Arg::Null),
        }
    });
    method(reg, rt, "DOMStringList", "contains", move |cx, w| {
        let id = this_of(cx, w, list)?;
        need_args(cx, 1, "contains", "DOMStringList")?;
        let s = cx.arg_string(0)?;
        cx.ret(items(w, id).contains(&s))
    });
    let rt2 = rt.clone();
    reg.indexed_getter(
        "DOMStringList",
        Rc::new(move |cx, i| {
            let Some(w) = rt2.window(cx.holder_realm()) else {
                return Ok(None);
            };
            let Some(h) = cx.holder_host().filter(|h| h.kind == K_OBJ) else {
                return Ok(None);
            };
            Ok(items(&w, h.id)
                .get(i as usize)
                .map(|s| Arg::String(s.clone())))
        }),
    );
    // IDBVersionChangeEvent
    ctor(reg, rt, "IDBVersionChangeEvent", |cx, w| {
        need_args(cx, 1, "construct", "IDBVersionChangeEvent").map_err(|_| {
            HostError::type_error(
                "Failed to construct 'IDBVersionChangeEvent': 1 argument required, but only 0 present.",
            )
        })?;
        let type_ = cx.arg_string(0)?;
        let d = super::Dict::arg(cx, 1);
        let mut e = super::events::EventData::new(
            "IDBVersionChangeEvent",
            &type_,
            d.bool(cx, "bubbles")?,
            d.bool(cx, "cancelable")?,
        );
        let old = d.number(cx, "oldVersion")?.unwrap_or(0.0);
        let new = match d.get(cx, "newVersion")? {
            Some(h) => {
                let v = cx.handle_value(h);
                cx.release(h);
                match v {
                    JsValue::Null | JsValue::Undefined => None,
                    _ => d.number(cx, "newVersion")?,
                }
            }
            None => None,
        };
        e.extra = EventExtra::VersionChange {
            old: old as u64,
            new: new.map(|n| n as u64),
        };
        d.release(cx);
        e.time = super::events::coarse_now(w);
        let id = w.new_obj(Obj::Event(Box::new(e)));
        cx.init_this(HostRef::new(K_OBJ, id))
    });
}

/// The parent of an IndexedDB event target in an event path: request →
/// transaction → database.
pub(crate) fn event_parent(w: &Window, item: HostRef) -> Option<HostRef> {
    if item.kind != K_OBJ {
        return None;
    }
    let p = with(w, item.id, |o| match o {
        IdbObj::Request(r) if !r.open => r.tx,
        IdbObj::Transaction(t) => Some(t.db),
        _ => None,
    })
    .flatten()?;
    Some(HostRef::new(K_OBJ, p))
}

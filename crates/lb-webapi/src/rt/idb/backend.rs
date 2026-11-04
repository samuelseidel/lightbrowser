//! The IndexedDB backend of one browser context (ADR 0028): databases per
//! origin in memory (nothing on disk), connections, the open/delete
//! queue with `versionchange`/`blocked`, transaction scheduling and the
//! data operations. Shared by every window and worker of the context
//! (`Send`; one lock). Results reach a window through its [`Sink`] as
//! tasks, in the order the backend produced them, like Chrome's IPC
//! replies.
//!
//! Requests of a started transaction run when they are placed (the
//! renderer saw them in order and nothing else touches the scope); the
//! window fires their events later, from its task queue. A read-write or
//! version change transaction keeps a copy of its scope (the whole
//! database for a version change) to roll back an abort.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::ops::Bound;
use std::sync::{Arc, Mutex};

use super::key::{Key, KeyPath, Range};

/// A structured clone as stored: V8's wire bytes, the array buffers it
/// refers to, and the blobs (bytes, type) of `Blob` host objects.
#[derive(Clone, Debug, Default)]
pub struct Value {
    pub data: Vec<u8>,
    pub buffers: Vec<Vec<u8>>,
    pub blobs: Vec<(Vec<u8>, String)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct IndexMeta {
    pub name: String,
    pub key_path: KeyPath,
    pub unique: bool,
    pub multi_entry: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StoreMeta {
    pub name: String,
    pub key_path: Option<KeyPath>,
    pub auto_increment: bool,
    pub indexes: Vec<IndexMeta>,
}

/// What a connection knows of its database.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DbMeta {
    pub name: String,
    pub version: u64,
    pub stores: Vec<StoreMeta>,
}

#[derive(Clone, Debug)]
struct Record {
    value: Value,
    /// Index keys of this record by index name.
    index_keys: HashMap<String, Vec<Key>>,
}

#[derive(Clone, Debug)]
struct Index {
    meta: IndexMeta,
    /// (index key, primary key).
    entries: BTreeSet<(Key, Key)>,
}

#[derive(Clone, Debug)]
struct Store {
    meta: StoreMeta,
    records: BTreeMap<Key, Record>,
    indexes: BTreeMap<String, Index>,
    /// Key generator's current number; `INFINITY` once past 2^53.
    current: f64,
}

#[derive(Clone, Debug)]
struct Database {
    version: u64,
    stores: BTreeMap<String, Store>,
}

/// Cursor direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Next,
    NextUnique,
    Prev,
    PrevUnique,
}

impl Dir {
    pub fn parse(s: &str) -> Option<Dir> {
        Some(match s {
            "next" => Dir::Next,
            "nextunique" => Dir::NextUnique,
            "prev" => Dir::Prev,
            "prevunique" => Dir::PrevUnique,
            _ => return None,
        })
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Dir::Next => "next",
            Dir::NextUnique => "nextunique",
            Dir::Prev => "prev",
            Dir::PrevUnique => "prevunique",
        }
    }
    fn forward(self) -> bool {
        matches!(self, Dir::Next | Dir::NextUnique)
    }
    fn unique(self) -> bool {
        matches!(self, Dir::NextUnique | Dir::PrevUnique)
    }
}

/// How a cursor moves from its position.
#[derive(Clone, Debug)]
pub enum Step {
    /// The first record (opening).
    First,
    /// `advance(n)` (`continue()` is `Count(1)`).
    Count(u32),
    /// `continue(key)`.
    Key(Key),
    /// `continuePrimaryKey(key, primaryKey)`.
    KeyPrimary(Key, Key),
}

/// What `getAll`-style requests return.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AllKind {
    Values,
    Keys,
    Records,
}

/// A data request of a transaction.
#[derive(Clone, Debug)]
pub enum Op {
    Put {
        store: String,
        value: Value,
        key: Option<Key>,
        add: bool,
        index_keys: Vec<(String, Vec<Key>)>,
        /// Indexes whose key needs the generated primary key: `None` is
        /// the key itself (index on the store's key path), `Some(parts)`
        /// an array key path with the primary key at the `None` parts.
        pk_indexes: Vec<(String, Option<Vec<Option<Key>>>)>,
    },
    Get {
        store: String,
        index: Option<String>,
        range: Range,
        key_only: bool,
    },
    GetAll {
        store: String,
        index: Option<String>,
        range: Range,
        count: Option<u32>,
        dir: Dir,
        kind: AllKind,
    },
    Count {
        store: String,
        index: Option<String>,
        range: Range,
    },
    Delete {
        store: String,
        range: Range,
    },
    Clear {
        store: String,
    },
    Cursor {
        store: String,
        index: Option<String>,
        range: Range,
        dir: Dir,
        key_only: bool,
        /// Current (key, primary key); `None` before the first step.
        pos: Option<(Key, Key)>,
        step: Step,
    },
}

/// A cursor position.
#[derive(Clone, Debug)]
pub struct CursorPos {
    pub key: Key,
    pub primary_key: Key,
    pub value: Option<Value>,
}

/// Result of an [`Op`].
#[derive(Clone, Debug)]
pub enum OpResult {
    Key(Key),
    Undefined,
    /// A value with its primary key (for key injection).
    Value(Option<(Value, Key)>),
    Keys(Vec<Key>),
    Values(Vec<(Value, Key)>),
    /// (key, primary key, value) of `getAllRecords`.
    Records(Vec<(Key, Key, Value)>),
    Count(u64),
    Cursor(Option<CursorPos>),
    Error {
        name: &'static str,
        message: String,
    },
}

/// What the backend tells a window.
pub enum Ev {
    /// Request `req` (a window object id) finished.
    Result {
        txn: u64,
        req: u32,
        result: OpResult,
        /// Key path to inject the primary key at (key generator stores).
        inject: Option<String>,
    },
    /// Transaction `txn` committed.
    Complete { txn: u64 },
    /// Another connection wants a new version (or to delete: `new` none).
    VersionChange {
        conn: u64,
        old: u64,
        new: Option<u64>,
    },
    /// The open/delete request `req` waits for other connections.
    Blocked {
        req: u32,
        old: u64,
        new: Option<u64>,
    },
    /// Open request `req` runs an upgrade.
    Upgrade {
        req: u32,
        conn: u64,
        txn: u64,
        old: u64,
        meta: DbMeta,
    },
    /// Open request `req` succeeded.
    Opened { req: u32, conn: u64, meta: DbMeta },
    /// Open request `req` failed.
    OpenFailed {
        req: u32,
        name: &'static str,
        message: String,
    },
    /// Delete request `req` succeeded.
    Deleted { req: u32, old: u64 },
}

/// Delivers [`Ev`]s to one window (a task in its queue).
pub type Sink = Arc<dyn Fn(Ev) + Send + Sync>;

enum Pending {
    Open {
        req: u32,
        version: Option<u64>,
        sink: Sink,
    },
    Delete {
        req: u32,
        sink: Sink,
    },
}

impl Pending {
    fn req(&self) -> (u32, &Sink) {
        match self {
            Pending::Open { req, sink, .. } | Pending::Delete { req, sink } => (*req, sink),
        }
    }
}

enum Phase {
    /// Waiting for the other connections' `versionchange` handlers.
    Acks {
        left: Vec<u64>,
        old: u64,
        new: Option<u64>,
    },
    /// `blocked` fired; waiting for the connections to close.
    Closing,
    /// The version change transaction runs.
    Upgrading { req: u32 },
}

struct Conn {
    id: u64,
    sink: Sink,
    close_pending: bool,
}

#[derive(Default)]
struct Entry {
    db: Option<Database>,
    conns: Vec<Conn>,
    queue: VecDeque<Pending>,
    phase: Option<Phase>,
    /// Unfinished transactions in creation order.
    txns: Vec<u64>,
    /// The database is being created by a running upgrade.
    creating: bool,
    /// Version before a running upgrade (what `databases()` reports).
    upgrading_from: Option<u64>,
}

enum Snapshot {
    Stores(Vec<(String, Option<Store>)>),
    Db(Option<Database>),
}

struct Txn {
    origin: String,
    name: String,
    conn: u64,
    mode: Mode,
    scope: Vec<String>,
    started: bool,
    queue: VecDeque<(u32, Op)>,
    commit_requested: bool,
    snapshot: Option<Snapshot>,
    sink: Sink,
}

/// Transaction mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    ReadOnly,
    ReadWrite,
    VersionChange,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::ReadOnly => "readonly",
            Mode::ReadWrite => "readwrite",
            Mode::VersionChange => "versionchange",
        }
    }
}

#[derive(Default)]
struct State {
    /// Origin → database name → entry.
    origins: HashMap<String, BTreeMap<String, Entry>>,
    txns: HashMap<u64, Txn>,
    /// Connection id → (origin, name).
    conn_db: HashMap<u64, (String, String)>,
    next: u64,
}

/// The IndexedDB backend of one browser context. Cheap to clone.
#[derive(Clone, Default)]
pub struct IdbBackend(Arc<Mutex<State>>);

impl std::fmt::Debug for IdbBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("IdbBackend")
    }
}

const MAX_GENERATED: f64 = 9_007_199_254_740_992.0;

fn store_meta_list(db: &Database) -> Vec<StoreMeta> {
    db.stores.values().map(|s| s.meta.clone()).collect()
}

fn meta_of(name: &str, db: &Database) -> DbMeta {
    DbMeta {
        name: name.to_owned(),
        version: db.version,
        stores: store_meta_list(db),
    }
}

impl IdbBackend {
    pub fn new() -> IdbBackend {
        IdbBackend::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `(name, version)` of the databases of `origin` (`databases()`).
    pub fn databases(&self, origin: &str) -> Vec<(String, u64)> {
        let st = self.lock();
        st.origins
            .get(origin)
            .map(|m| {
                m.iter()
                    .filter(|(_, e)| !e.creating)
                    .filter_map(|(n, e)| {
                        e.db.as_ref()
                            .map(|d| (n.clone(), e.upgrading_from.unwrap_or(d.version)))
                    })
                    .filter(|(_, v)| *v > 0)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// `indexedDB.open(name, version)`.
    pub fn open(&self, origin: &str, name: &str, version: Option<u64>, req: u32, sink: Sink) {
        let mut st = self.lock();
        let e = entry(&mut st, origin, name);
        e.queue.push_back(Pending::Open { req, version, sink });
        process(&mut st, origin, name);
    }

    /// `indexedDB.deleteDatabase(name)`.
    pub fn delete(&self, origin: &str, name: &str, req: u32, sink: Sink) {
        let mut st = self.lock();
        let e = entry(&mut st, origin, name);
        e.queue.push_back(Pending::Delete { req, sink });
        process(&mut st, origin, name);
    }

    /// A connection's window ran its `versionchange` event.
    pub fn version_change_handled(&self, conn: u64) {
        let mut st = self.lock();
        let Some((o, n)) = st.conn_db.get(&conn).cloned() else {
            return;
        };
        if let Some(e) = st.origins.get_mut(&o).and_then(|m| m.get_mut(&n))
            && let Some(Phase::Acks { left, .. }) = &mut e.phase
        {
            left.retain(|c| *c != conn);
        }
        process(&mut st, &o, &n);
    }

    /// `db.close()`: the connection closes once its transactions finished.
    pub fn close(&self, conn: u64) {
        let mut st = self.lock();
        let Some((o, n)) = st.conn_db.get(&conn).cloned() else {
            return;
        };
        if let Some(e) = st.origins.get_mut(&o).and_then(|m| m.get_mut(&n))
            && let Some(c) = e.conns.iter_mut().find(|c| c.id == conn)
        {
            c.close_pending = true;
        }
        maybe_close(&mut st, conn);
        process(&mut st, &o, &n);
    }

    /// Start a transaction on connection `conn`. Returns its id.
    pub fn transaction(&self, conn: u64, scope: Vec<String>, mode: Mode, sink: Sink) -> u64 {
        let mut st = self.lock();
        st.next += 1;
        let id = st.next;
        let (o, n) = st.conn_db.get(&conn).cloned().unwrap_or_default();
        st.txns.insert(
            id,
            Txn {
                origin: o.clone(),
                name: n.clone(),
                conn,
                mode,
                scope,
                started: false,
                queue: VecDeque::new(),
                commit_requested: false,
                snapshot: None,
                sink,
            },
        );
        if let Some(e) = st.origins.get_mut(&o).and_then(|m| m.get_mut(&n)) {
            e.txns.push(id);
        }
        try_start(&mut st, &o, &n);
        id
    }

    /// Place a request (`req` is the window's request object).
    pub fn request(&self, txn: u64, req: u32, op: Op) {
        let mut st = self.lock();
        let Some(t) = st.txns.get_mut(&txn) else {
            return;
        };
        if t.started {
            run_op(&mut st, txn, req, op);
        } else {
            t.queue.push_back((req, op));
        }
    }

    /// Commit (no requests are left on the window's side).
    pub fn commit(&self, txn: u64) {
        let mut st = self.lock();
        let Some(t) = st.txns.get_mut(&txn) else {
            return;
        };
        if !t.started || !t.queue.is_empty() {
            t.commit_requested = true;
            return;
        }
        finish(&mut st, txn, true);
    }

    /// Abort: roll the scope back. The window fires the events.
    pub fn abort(&self, txn: u64) {
        let mut st = self.lock();
        if st.txns.contains_key(&txn) {
            finish(&mut st, txn, false);
        }
    }

    /// Records of a store in the version change transaction `txn`, for
    /// building a new index (the window computes the index keys).
    pub fn store_values(&self, txn: u64, store: &str) -> Vec<(Key, Value)> {
        let st = self.lock();
        with_db(&st, txn)
            .and_then(|d| d.stores.get(store))
            .map(|s| {
                s.records
                    .iter()
                    .map(|(k, r)| (k.clone(), r.value.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Schema change in version change transaction `txn`. `Err` is a
    /// `ConstraintError` message (a unique index over duplicates).
    pub fn schema(&self, txn: u64, change: Schema) -> Result<(), String> {
        let mut st = self.lock();
        let Some(db) = with_db_mut(&mut st, txn) else {
            return Ok(());
        };
        match change {
            Schema::CreateStore(meta) => {
                db.stores.insert(
                    meta.name.clone(),
                    Store {
                        meta,
                        records: BTreeMap::new(),
                        indexes: BTreeMap::new(),
                        current: 1.0,
                    },
                );
            }
            Schema::DeleteStore(name) => {
                db.stores.remove(&name);
            }
            Schema::RenameStore(old, new) => {
                if let Some(mut s) = db.stores.remove(&old) {
                    s.meta.name = new.clone();
                    db.stores.insert(new, s);
                }
            }
            Schema::CreateIndex(store, meta, keys) => {
                let Some(s) = db.stores.get_mut(&store) else {
                    return Ok(());
                };
                let mut entries = BTreeSet::new();
                let mut fail = false;
                for (pk, ks) in &keys {
                    for k in ks {
                        if meta.unique && entries.iter().any(|(ek, _): &(Key, Key)| ek == k) {
                            fail = true;
                        }
                        entries.insert((k.clone(), pk.clone()));
                    }
                    if let Some(r) = s.records.get_mut(pk) {
                        r.index_keys.insert(meta.name.clone(), ks.clone());
                    }
                }
                let name = meta.name.clone();
                s.meta.indexes.retain(|i| i.name != name);
                s.meta.indexes.push(meta.clone());
                s.meta.indexes.sort_by(|a, b| a.name.cmp(&b.name));
                s.indexes.insert(name.clone(), Index { meta, entries });
                if fail {
                    return Err(format!(
                        "Unable to add key to index '{name}': at least one key does not satisfy the uniqueness requirements."
                    ));
                }
            }
            Schema::DeleteIndex(store, name) => {
                if let Some(s) = db.stores.get_mut(&store) {
                    s.indexes.remove(&name);
                    s.meta.indexes.retain(|i| i.name != name);
                    for r in s.records.values_mut() {
                        r.index_keys.remove(&name);
                    }
                }
            }
            Schema::RenameIndex(store, old, new) => {
                if let Some(s) = db.stores.get_mut(&store)
                    && let Some(mut i) = s.indexes.remove(&old)
                {
                    i.meta.name = new.clone();
                    for m in &mut s.meta.indexes {
                        if m.name == old {
                            m.name = new.clone();
                        }
                    }
                    s.meta.indexes.sort_by(|a, b| a.name.cmp(&b.name));
                    for r in s.records.values_mut() {
                        if let Some(k) = r.index_keys.remove(&old) {
                            r.index_keys.insert(new.clone(), k);
                        }
                    }
                    s.indexes.insert(new, i);
                }
            }
        }
        Ok(())
    }
}

/// A schema change of a version change transaction.
pub enum Schema {
    CreateStore(StoreMeta),
    DeleteStore(String),
    RenameStore(String, String),
    /// Store, index, and the index keys of every record (primary key →
    /// keys).
    CreateIndex(String, IndexMeta, Vec<(Key, Vec<Key>)>),
    DeleteIndex(String, String),
    RenameIndex(String, String, String),
}

fn entry<'a>(st: &'a mut State, origin: &str, name: &str) -> &'a mut Entry {
    st.origins
        .entry(origin.to_owned())
        .or_default()
        .entry(name.to_owned())
        .or_default()
}

fn with_db(st: &State, txn: u64) -> Option<&Database> {
    let t = st.txns.get(&txn)?;
    st.origins.get(&t.origin)?.get(&t.name)?.db.as_ref()
}

fn with_db_mut(st: &mut State, txn: u64) -> Option<&mut Database> {
    let (o, n) = {
        let t = st.txns.get(&txn)?;
        (t.origin.clone(), t.name.clone())
    };
    st.origins.get_mut(&o)?.get_mut(&n)?.db.as_mut()
}

/// Advance the open/delete queue of a database.
fn process(st: &mut State, origin: &str, name: &str) {
    loop {
        let Some(e) = st.origins.get_mut(origin).and_then(|m| m.get_mut(name)) else {
            return;
        };
        match &e.phase {
            Some(Phase::Upgrading { .. }) => return,
            Some(Phase::Acks { left, old, new }) => {
                if !left.is_empty() {
                    return;
                }
                let (old, new) = (*old, *new);
                if e.conns.is_empty() {
                    e.phase = None;
                } else {
                    if let Some(p) = e.queue.front() {
                        let (req, sink) = p.req();
                        sink(Ev::Blocked { req, old, new });
                    }
                    e.phase = Some(Phase::Closing);
                    return;
                }
            }
            Some(Phase::Closing) => {
                if !e.conns.is_empty() {
                    return;
                }
                e.phase = None;
            }
            None => {}
        }
        // Phase is idle: take the next request.
        let Some(p) = e.queue.front() else {
            e.phase = None;
            return;
        };
        let current = e.db.as_ref().map_or(0, |d| d.version);
        match p {
            Pending::Open { req, version, sink } => {
                let (req, sink) = (*req, sink.clone());
                let target = version.unwrap_or(if current == 0 { 1 } else { current });
                if target < current {
                    e.queue.pop_front();
                    sink(Ev::OpenFailed {
                        req,
                        name: "VersionError",
                        message: format!(
                            "The requested version ({target}) is less than the existing version ({current})."
                        ),
                    });
                    continue;
                }
                if target == current {
                    e.queue.pop_front();
                    st.next += 1;
                    let conn = st.next;
                    let e = st
                        .origins
                        .get_mut(origin)
                        .and_then(|m| m.get_mut(name))
                        .expect("entry");
                    e.conns.push(Conn {
                        id: conn,
                        sink: sink.clone(),
                        close_pending: false,
                    });
                    let meta = meta_of(name, e.db.as_ref().expect("db"));
                    st.conn_db
                        .insert(conn, (origin.to_owned(), name.to_owned()));
                    sink(Ev::Opened { req, conn, meta });
                    continue;
                }
                // An upgrade: other connections first.
                if e.phase.is_none() && !e.conns.is_empty() {
                    let left: Vec<u64> = e
                        .conns
                        .iter()
                        .filter(|c| !c.close_pending)
                        .map(|c| c.id)
                        .collect();
                    for c in e.conns.iter().filter(|c| !c.close_pending) {
                        (c.sink)(Ev::VersionChange {
                            conn: c.id,
                            old: current,
                            new: Some(target),
                        });
                    }
                    e.phase = Some(Phase::Acks {
                        left,
                        old: current,
                        new: Some(target),
                    });
                    return;
                }
                start_upgrade(st, origin, name, req, sink, target);
                return;
            }
            Pending::Delete { req, sink } => {
                let (req, sink) = (*req, sink.clone());
                if e.phase.is_none() && !e.conns.is_empty() {
                    let left: Vec<u64> = e
                        .conns
                        .iter()
                        .filter(|c| !c.close_pending)
                        .map(|c| c.id)
                        .collect();
                    for c in e.conns.iter().filter(|c| !c.close_pending) {
                        (c.sink)(Ev::VersionChange {
                            conn: c.id,
                            old: current,
                            new: None,
                        });
                    }
                    e.phase = Some(Phase::Acks {
                        left,
                        old: current,
                        new: None,
                    });
                    return;
                }
                e.queue.pop_front();
                e.db = None;
                e.phase = None;
                sink(Ev::Deleted { req, old: current });
            }
        }
    }
}

fn start_upgrade(st: &mut State, origin: &str, name: &str, req: u32, sink: Sink, target: u64) {
    st.next += 1;
    let conn = st.next;
    st.next += 1;
    let txn = st.next;
    let e = st
        .origins
        .get_mut(origin)
        .and_then(|m| m.get_mut(name))
        .expect("entry");
    e.queue.pop_front();
    let before = e.db.clone();
    e.creating = before.is_none();
    let old = before.as_ref().map_or(0, |d| d.version);
    e.upgrading_from = Some(old);
    let db = e.db.get_or_insert_with(|| Database {
        version: 0,
        stores: BTreeMap::new(),
    });
    let mut meta = meta_of(name, db);
    db.version = target;
    meta.version = target;
    e.conns.push(Conn {
        id: conn,
        sink: sink.clone(),
        close_pending: false,
    });
    e.phase = Some(Phase::Upgrading { req });
    e.txns.push(txn);
    let scope = meta.stores.iter().map(|s| s.name.clone()).collect();
    st.conn_db
        .insert(conn, (origin.to_owned(), name.to_owned()));
    st.txns.insert(
        txn,
        Txn {
            origin: origin.to_owned(),
            name: name.to_owned(),
            conn,
            mode: Mode::VersionChange,
            scope,
            started: true,
            queue: VecDeque::new(),
            commit_requested: false,
            snapshot: Some(Snapshot::Db(before)),
            sink: sink.clone(),
        },
    );
    sink(Ev::Upgrade {
        req,
        conn,
        txn,
        old,
        meta,
    });
}

fn overlaps(a: &[String], b: &[String]) -> bool {
    a.iter().any(|x| b.contains(x))
}

/// Start every waiting transaction of a database that may run.
fn try_start(st: &mut State, origin: &str, name: &str) {
    let Some(e) = st.origins.get(origin).and_then(|m| m.get(name)) else {
        return;
    };
    let order = e.txns.clone();
    let mut startable = Vec::new();
    for (i, id) in order.iter().enumerate() {
        let Some(t) = st.txns.get(id) else { continue };
        if t.started {
            continue;
        }
        let blocked = order[..i].iter().any(|p| {
            st.txns.get(p).is_some_and(|pt| {
                (pt.mode != Mode::ReadOnly || t.mode != Mode::ReadOnly)
                    && (pt.mode == Mode::VersionChange || overlaps(&pt.scope, &t.scope))
            })
        });
        if !blocked {
            startable.push(*id);
        }
    }
    for id in startable {
        // A transaction finished above may have started (and finished)
        // this one already.
        if st.txns.get(&id).is_none_or(|t| t.started) {
            continue;
        }
        let snapshot = {
            let t = &st.txns[&id];
            (t.mode == Mode::ReadWrite).then(|| {
                let db = st
                    .origins
                    .get(&t.origin)
                    .and_then(|m| m.get(&t.name))
                    .and_then(|e| e.db.as_ref());
                Snapshot::Stores(
                    t.scope
                        .iter()
                        .map(|s| (s.clone(), db.and_then(|d| d.stores.get(s).cloned())))
                        .collect(),
                )
            })
        };
        let Some(t) = st.txns.get_mut(&id) else {
            continue;
        };
        t.started = true;
        t.snapshot = snapshot;
        let queue: Vec<(u32, Op)> = t.queue.drain(..).collect();
        for (req, op) in queue {
            run_op(st, id, req, op);
        }
        if st.txns.get(&id).is_some_and(|t| t.commit_requested) {
            finish(st, id, true);
        }
    }
}

/// End a transaction: keep (commit) or roll back its changes.
fn finish(st: &mut State, txn: u64, commit: bool) {
    let Some(t) = st.txns.remove(&txn) else {
        return;
    };
    let (o, n) = (t.origin.clone(), t.name.clone());
    if let Some(e) = st.origins.get_mut(&o).and_then(|m| m.get_mut(&n)) {
        e.txns.retain(|x| *x != txn);
        if !commit {
            match t.snapshot {
                Some(Snapshot::Db(before)) => e.db = before,
                Some(Snapshot::Stores(stores)) => {
                    if let Some(db) = e.db.as_mut() {
                        for (name, s) in stores {
                            match s {
                                Some(s) => {
                                    db.stores.insert(name, s);
                                }
                                None => {
                                    db.stores.remove(&name);
                                }
                            }
                        }
                    }
                }
                None => {}
            }
        }
        if t.mode == Mode::VersionChange {
            let req = match e.phase {
                Some(Phase::Upgrading { req, .. }) => req,
                _ => u32::MAX,
            };
            e.phase = None;
            e.creating = false;
            e.upgrading_from = None;
            if commit {
                (t.sink)(Ev::Complete { txn });
                let meta = e.db.as_ref().map(|d| meta_of(&n, d)).unwrap_or_default();
                // The open request succeeds after `complete`.
                (t.sink)(Ev::Opened {
                    req,
                    conn: t.conn,
                    meta,
                });
            } else {
                // The connection of an aborted upgrade is closed.
                e.conns.retain(|c| c.id != t.conn);
                st.conn_db.remove(&t.conn);
            }
        } else if commit {
            (t.sink)(Ev::Complete { txn });
        }
    }
    maybe_close(st, t.conn);
    try_start(st, &o, &n);
    process(st, &o, &n);
}

/// Close connection `conn` if it asked to close and has no transactions.
fn maybe_close(st: &mut State, conn: u64) {
    let Some((o, n)) = st.conn_db.get(&conn).cloned() else {
        return;
    };
    let busy = st.txns.values().any(|t| t.conn == conn);
    if busy {
        return;
    }
    if let Some(e) = st.origins.get_mut(&o).and_then(|m| m.get_mut(&n)) {
        let closing = e.conns.iter().any(|c| c.id == conn && c.close_pending);
        if closing {
            e.conns.retain(|c| c.id != conn);
            if let Some(Phase::Acks { left, .. }) = &mut e.phase {
                left.retain(|c| *c != conn);
            }
            st.conn_db.remove(&conn);
        }
    }
}

fn error(name: &'static str, message: impl Into<String>) -> OpResult {
    OpResult::Error {
        name,
        message: message.into(),
    }
}

fn run_op(st: &mut State, txn: u64, req: u32, op: Op) {
    let Some(sink) = st.txns.get(&txn).map(|t| t.sink.clone()) else {
        return;
    };
    let (result, inject) = match with_db_mut(st, txn) {
        Some(db) => exec(db, op),
        None => (error("UnknownError", "The database was deleted."), None),
    };
    sink(Ev::Result {
        txn,
        req,
        result,
        inject,
    });
}

fn inject_path(s: &Store) -> Option<String> {
    match (&s.meta.key_path, s.meta.auto_increment) {
        (Some(KeyPath::String(p)), true) => Some(p.clone()),
        _ => None,
    }
}

/// Records of `s` (or of its index `index`) in `range` and direction, as
/// (key, primary key).
fn scan(s: &Store, index: Option<&str>, range: &Range, dir: Dir) -> Vec<(Key, Key)> {
    let mut out: Vec<(Key, Key)> = match index.and_then(|i| s.indexes.get(i)) {
        Some(ix) => ix
            .entries
            .iter()
            .filter(|(k, _)| range.contains(k))
            .cloned()
            .collect(),
        None => {
            let lo = match &range.lower {
                Some(l) if range.lower_open => Bound::Excluded(l.clone()),
                Some(l) => Bound::Included(l.clone()),
                None => Bound::Unbounded,
            };
            let hi = match &range.upper {
                Some(u) if range.upper_open => Bound::Excluded(u.clone()),
                Some(u) => Bound::Included(u.clone()),
                None => Bound::Unbounded,
            };
            if matches!((&lo, &hi), (Bound::Included(a) | Bound::Excluded(a), Bound::Included(b) | Bound::Excluded(b)) if a > b)
            {
                Vec::new()
            } else {
                s.records
                    .range((lo, hi))
                    .map(|(k, _)| (k.clone(), k.clone()))
                    .collect()
            }
        }
    };
    if !dir.forward() {
        out.reverse();
    }
    if dir.unique() {
        let mut seen: Vec<(Key, Key)> = Vec::new();
        if dir.forward() {
            for e in out {
                if seen.last().is_none_or(|l| l.0 != e.0) {
                    seen.push(e);
                }
            }
        } else {
            // prevunique: the first (lowest primary key) of each key.
            for e in out {
                match seen.last_mut() {
                    Some(l) if l.0 == e.0 => *l = e,
                    _ => seen.push(e),
                }
            }
        }
        out = seen;
    }
    out
}

fn exec(db: &mut Database, op: Op) -> (OpResult, Option<String>) {
    match op {
        Op::Put {
            store,
            value,
            key,
            add,
            mut index_keys,
            pk_indexes,
        } => {
            let Some(s) = db.stores.get_mut(&store) else {
                return (
                    error("NotFoundError", "The object store was deleted."),
                    None,
                );
            };
            // The generator only moves when the put succeeds (Chrome).
            let mut next_current = s.current;
            let key = match key {
                Some(k) => {
                    if let Key::Number(n) = k
                        && s.meta.auto_increment
                        && n >= s.current
                    {
                        next_current = if n >= MAX_GENERATED {
                            f64::INFINITY
                        } else {
                            n.floor() + 1.0
                        };
                    }
                    k
                }
                None => {
                    if s.current > MAX_GENERATED {
                        return (
                            error("ConstraintError", "Maximum key generator value reached."),
                            None,
                        );
                    }
                    next_current = s.current + 1.0;
                    Key::Number(s.current)
                }
            };
            for (name, parts) in pk_indexes {
                let k = match parts {
                    None => key.clone(),
                    Some(parts) => Key::Array(
                        parts
                            .into_iter()
                            .map(|p| p.unwrap_or_else(|| key.clone()))
                            .collect(),
                    ),
                };
                index_keys.retain(|(n, _)| *n != name);
                index_keys.push((name, vec![k]));
            }
            if add && s.records.contains_key(&key) {
                return (
                    error("ConstraintError", "Key already exists in the object store."),
                    None,
                );
            }
            // Unique indexes.
            for (name, keys) in &index_keys {
                let Some(ix) = s.indexes.get(name) else {
                    continue;
                };
                if !ix.meta.unique {
                    continue;
                }
                for k in keys {
                    let clash = ix
                        .entries
                        .range((k.clone(), Key::Number(f64::NEG_INFINITY))..)
                        .take_while(|(ek, _)| ek == k)
                        .any(|(_, pk)| *pk != key);
                    if clash {
                        return (
                            error(
                                "ConstraintError",
                                format!(
                                    "Unable to add key to index '{name}': at least one key does not satisfy the uniqueness requirements."
                                ),
                            ),
                            None,
                        );
                    }
                }
            }
            if let Some(old) = s.records.remove(&key) {
                for (name, keys) in old.index_keys {
                    if let Some(ix) = s.indexes.get_mut(&name) {
                        for k in keys {
                            ix.entries.remove(&(k, key.clone()));
                        }
                    }
                }
            }
            let mut map = HashMap::new();
            for (name, keys) in index_keys {
                if let Some(ix) = s.indexes.get_mut(&name) {
                    for k in &keys {
                        ix.entries.insert((k.clone(), key.clone()));
                    }
                    map.insert(name, keys);
                }
            }
            s.records.insert(
                key.clone(),
                Record {
                    value,
                    index_keys: map,
                },
            );
            s.current = next_current;
            (OpResult::Key(key), None)
        }
        Op::Get {
            store,
            index,
            range,
            key_only,
        } => {
            let Some(s) = db.stores.get(&store) else {
                return (
                    error("NotFoundError", "The object store was deleted."),
                    None,
                );
            };
            let first = scan(s, index.as_deref(), &range, Dir::Next)
                .into_iter()
                .next();
            let inject = inject_path(s);
            match first {
                None => (OpResult::Undefined, None),
                Some((_, pk)) if key_only => (OpResult::Key(pk), None),
                Some((_, pk)) => {
                    let v = s.records.get(&pk).map(|r| (r.value.clone(), pk));
                    (OpResult::Value(v), inject)
                }
            }
        }
        Op::GetAll {
            store,
            index,
            range,
            count,
            dir,
            kind,
        } => {
            let Some(s) = db.stores.get(&store) else {
                return (
                    error("NotFoundError", "The object store was deleted."),
                    None,
                );
            };
            let mut rows = scan(s, index.as_deref(), &range, dir);
            if let Some(c) = count.filter(|c| *c > 0) {
                rows.truncate(c as usize);
            }
            let inject = inject_path(s);
            match kind {
                AllKind::Keys => (
                    OpResult::Keys(rows.into_iter().map(|(_, pk)| pk).collect()),
                    None,
                ),
                AllKind::Values => (
                    OpResult::Values(
                        rows.into_iter()
                            .filter_map(|(_, pk)| s.records.get(&pk).map(|r| (r.value.clone(), pk)))
                            .collect(),
                    ),
                    inject,
                ),
                AllKind::Records => (
                    OpResult::Records(
                        rows.into_iter()
                            .filter_map(|(k, pk)| {
                                s.records.get(&pk).map(|r| (k, pk.clone(), r.value.clone()))
                            })
                            .collect(),
                    ),
                    inject,
                ),
            }
        }
        Op::Count {
            store,
            index,
            range,
        } => {
            let Some(s) = db.stores.get(&store) else {
                return (
                    error("NotFoundError", "The object store was deleted."),
                    None,
                );
            };
            (
                OpResult::Count(scan(s, index.as_deref(), &range, Dir::Next).len() as u64),
                None,
            )
        }
        Op::Delete { store, range } => {
            let Some(s) = db.stores.get_mut(&store) else {
                return (
                    error("NotFoundError", "The object store was deleted."),
                    None,
                );
            };
            let keys: Vec<Key> = scan(s, None, &range, Dir::Next)
                .into_iter()
                .map(|(k, _)| k)
                .collect();
            for k in keys {
                if let Some(old) = s.records.remove(&k) {
                    for (name, ks) in old.index_keys {
                        if let Some(ix) = s.indexes.get_mut(&name) {
                            for ik in ks {
                                ix.entries.remove(&(ik, k.clone()));
                            }
                        }
                    }
                }
            }
            (OpResult::Undefined, None)
        }
        Op::Clear { store } => {
            let Some(s) = db.stores.get_mut(&store) else {
                return (
                    error("NotFoundError", "The object store was deleted."),
                    None,
                );
            };
            s.records.clear();
            for ix in s.indexes.values_mut() {
                ix.entries.clear();
            }
            (OpResult::Undefined, None)
        }
        Op::Cursor {
            store,
            index,
            range,
            dir,
            key_only,
            pos,
            step,
        } => {
            let Some(s) = db.stores.get(&store) else {
                return (
                    error("NotFoundError", "The object store was deleted."),
                    None,
                );
            };
            let rows = scan(s, index.as_deref(), &range, dir);
            let fwd = dir.forward();
            let is_index = index.is_some();
            // Position strictly after `pos` in iteration order.
            let after = |r: &(Key, Key), p: &(Key, Key)| -> bool {
                let o = if is_index && !dir.unique() {
                    (&r.0, &r.1).cmp(&(&p.0, &p.1))
                } else {
                    r.0.cmp(&p.0)
                };
                if fwd {
                    o == std::cmp::Ordering::Greater
                } else {
                    o == std::cmp::Ordering::Less
                }
            };
            let found = match (&pos, step) {
                (None, _) => rows.into_iter().next(),
                (Some(p), Step::First) | (Some(p), Step::Count(1)) => {
                    rows.into_iter().find(|r| after(r, p))
                }
                (Some(p), Step::Count(n)) => {
                    rows.into_iter().filter(|r| after(r, p)).nth(n as usize - 1)
                }
                (Some(p), Step::Key(k)) => rows
                    .into_iter()
                    .find(|r| after(r, p) && if fwd { r.0 >= k } else { r.0 <= k }),
                (Some(p), Step::KeyPrimary(k, pk)) => rows.into_iter().find(|r| {
                    after(r, p)
                        && if fwd {
                            (&r.0, &r.1) >= (&k, &pk)
                        } else {
                            (&r.0, &r.1) <= (&k, &pk)
                        }
                }),
            };
            let inject = inject_path(s);
            match found {
                None => (OpResult::Cursor(None), None),
                Some((k, pk)) => {
                    let value = if key_only {
                        None
                    } else {
                        s.records.get(&pk).map(|r| r.value.clone())
                    };
                    (
                        OpResult::Cursor(Some(CursorPos {
                            key: k,
                            primary_key: pk,
                            value,
                        })),
                        inject,
                    )
                }
            }
        }
    }
}

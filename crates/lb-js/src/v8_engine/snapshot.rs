//! V8 startup snapshot of the interface templates and one window context
//! (M6, ADR 0013).
//!
//! A snapshot holds:
//! - every interface's `FunctionTemplate` (and overflow `ObjectTemplate`)
//!   as isolate data, in definition order;
//! - one fully set up context: the global built from the window interface,
//!   every interface object instantiated and linked, overflow members
//!   defined and the host's init script (the installer fixup) run.
//!
//! Host callbacks are Rust closures and cannot be serialized. Templates
//! only carry a trampoline (an external reference, see [`external_refs`])
//! and an integer index into `State::callbacks`. Restoring therefore
//! replays the same `define_interface` calls: each spec is checked against
//! the digest recorded at creation, and only its callbacks are pushed, in
//! the order the template builder pushes them, so every index matches.
//! The first spec that differs (or arrives out of order) switches the
//! engine back to building templates, and the snapshot context is not
//! used. Nothing persona-specific can hide in a snapshot: values that
//! reach templates are part of the digest, and persona data is read by
//! the callbacks at call time.
//!
//! Blob layout: `MAGIC`, a little-endian header (see [`Meta`]), then V8's
//! startup data.

use std::borrow::Cow;

use super::*;

const MAGIC: &[u8; 8] = b"LBSNAP03";

/// Every native function a snapshot can reference, in a fixed order (the
/// index is what the snapshot stores). Both the creating and the restoring
/// isolate get this list.
pub(super) fn external_refs() -> Cow<'static, [v8::ExternalReference]> {
    use v8::{ExternalReference as R, MapFnTo};
    Cow::Owned(vec![
        R {
            function: host_trampoline.map_fn_to(),
        },
        R {
            function: ctor_trampoline.map_fn_to(),
        },
        R {
            named_getter: named_trampoline.map_fn_to(),
        },
        R {
            named_setter: named_set_trampoline.map_fn_to(),
        },
        R {
            named_deleter: named_delete_trampoline.map_fn_to(),
        },
        R {
            enumerator: named_enum_trampoline.map_fn_to(),
        },
        R {
            named_query: named_query_trampoline.map_fn_to(),
        },
        R {
            indexed_getter: indexed_trampoline.map_fn_to(),
        },
        R {
            indexed_query: indexed_query_trampoline.map_fn_to(),
        },
        R {
            enumerator: indexed_enum_trampoline.map_fn_to(),
        },
        R {
            indexed_setter: indexed_set_trampoline.map_fn_to(),
        },
        R {
            indexed_deleter: indexed_delete_trampoline.map_fn_to(),
        },
        R {
            pointer: {
                let f: v8::AccessorNameGetterCallback = lazy_interface_getter.map_fn_to();
                f as *mut std::ffi::c_void
            },
        },
    ])
}

/// What an engine knows about its snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotStatus {
    /// Built without a snapshot.
    None,
    /// A snapshot creator ([`Engine::snapshot_creator`]).
    Creating,
    /// A blob was given but could not be used; the engine built normally.
    Rejected(String),
    /// Restored from a blob. `replayed` of `total` interfaces came from it;
    /// `diverged` means a spec differed and the rest were built normally.
    /// `context` is whether new realms of the window interface are
    /// deserialized from the snapshot context.
    Restored {
        replayed: usize,
        total: usize,
        diverged: bool,
        context: bool,
    },
}

/// Header of a blob: what the creating engine recorded.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Meta {
    pub v8_version: String,
    pub v8_flags: String,
    /// The global interface of the snapshot context.
    pub window: u32,
    /// FNV-1a of the init script run in the context and of the base
    /// interface fixups (see [`init_digest`]).
    pub init_hash: u64,
    /// Interface objects made in the snapshot context.
    pub made: Vec<u32>,
    /// Per interface: spec digest, callback count before and after.
    pub digests: Vec<u64>,
    pub cb_before: Vec<u32>,
    pub cb_after: Vec<u32>,
    /// Per interface: isolate data index of the first overflow chunk
    /// template (`u32::MAX` if none) and the number of chunks, which sit
    /// at consecutive indexes.
    pub overflow_index: Vec<u32>,
    pub overflow_count: Vec<u32>,
}

impl Meta {
    fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(MAGIC);
        put_str(out, &self.v8_version);
        put_str(out, &self.v8_flags);
        put_u32(out, self.window);
        out.extend_from_slice(&self.init_hash.to_le_bytes());
        put_u32(out, self.made.len() as u32);
        for &id in &self.made {
            put_u32(out, id);
        }
        put_u32(out, self.digests.len() as u32);
        for i in 0..self.digests.len() {
            out.extend_from_slice(&self.digests[i].to_le_bytes());
            put_u32(out, self.cb_before[i]);
            put_u32(out, self.cb_after[i]);
            put_u32(out, self.overflow_index[i]);
            put_u32(out, self.overflow_count[i]);
        }
        // Keep V8's data 8-byte aligned relative to the blob start.
        while !out.len().is_multiple_of(8) {
            out.push(0);
        }
    }

    /// Parse a blob; returns the header and V8's part.
    pub(crate) fn decode(blob: &[u8]) -> Result<(Meta, &[u8]), String> {
        let mut r = Reader { b: blob, at: 0 };
        if r.take(8)? != MAGIC {
            return Err("not a LightBrowser snapshot (bad magic)".into());
        }
        let mut m = Meta {
            v8_version: r.str()?,
            v8_flags: r.str()?,
            window: r.u32()?,
            init_hash: r.u64()?,
            ..Meta::default()
        };
        let made = r.u32()? as usize;
        for _ in 0..made {
            m.made.push(r.u32()?);
        }
        let n = r.u32()? as usize;
        for _ in 0..n {
            m.digests.push(r.u64()?);
            m.cb_before.push(r.u32()?);
            m.cb_after.push(r.u32()?);
            m.overflow_index.push(r.u32()?);
            m.overflow_count.push(r.u32()?);
        }
        let at = r.at.next_multiple_of(8);
        let rest = blob.get(at..).ok_or("truncated snapshot")?;
        if rest.is_empty() {
            return Err("snapshot has no V8 data".into());
        }
        Ok((m, rest))
    }
}

fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    put_u32(out, s.len() as u32);
    out.extend_from_slice(s.as_bytes());
}

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let s = self
            .b
            .get(self.at..self.at + n)
            .ok_or("truncated snapshot header")?;
        self.at += n;
        Ok(s)
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().unwrap_or([0; 4]),
        ))
    }
    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().unwrap_or([0; 8]),
        ))
    }
    fn str(&mut self) -> Result<String, String> {
        let n = self.u32()? as usize;
        String::from_utf8(self.take(n)?.to_vec()).map_err(|_| "bad header string".into())
    }
}

/// Snapshot side of an engine's state.
pub(super) enum SnapState {
    Off,
    Rejected(String),
    Creating(Meta),
    Restoring(Box<Restore>),
}

pub(super) struct Restore {
    pub meta: Meta,
    /// Templates from the blob, taken as their interfaces are replayed.
    pub templates: Vec<Option<v8::Global<v8::FunctionTemplate>>>,
    pub overflows: Vec<Option<Vec<v8::Global<v8::ObjectTemplate>>>>,
    pub next: usize,
    pub diverged: bool,
    /// Set by [`Engine::use_snapshot_context`].
    pub context_ready: bool,
}

impl SnapState {
    pub(super) fn status(&self) -> SnapshotStatus {
        match self {
            SnapState::Off => SnapshotStatus::None,
            SnapState::Rejected(r) => SnapshotStatus::Rejected(r.clone()),
            SnapState::Creating(_) => SnapshotStatus::Creating,
            SnapState::Restoring(r) => SnapshotStatus::Restored {
                replayed: r.next,
                total: r.meta.digests.len(),
                diverged: r.diverged,
                context: r.context_ready,
            },
        }
    }

    /// The window interface whose realms come from the snapshot context.
    pub(super) fn context_global(&self) -> Option<InterfaceId> {
        match self {
            SnapState::Restoring(r) if r.context_ready => Some(InterfaceId(r.meta.window)),
            _ => None,
        }
    }

    /// Which interface objects a realm from the snapshot context starts
    /// with, by id.
    pub(super) fn context_made(&self) -> Vec<bool> {
        let SnapState::Restoring(r) = self else {
            return Vec::new();
        };
        let mut made = vec![false; r.meta.digests.len()];
        for &id in &r.meta.made {
            if let Some(m) = made.get_mut(id as usize) {
                *m = true;
            }
        }
        made
    }
}

// ---------------------------------------------------------------------------
// Digest of a spec (FNV-1a 64)
// ---------------------------------------------------------------------------

pub(super) struct Fnv(u64);

impl Fnv {
    pub(super) fn new() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
    fn bytes(&mut self, b: &[u8]) {
        for &x in b {
            self.0 ^= u64::from(x);
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
    }
    fn u32(&mut self, v: u32) {
        self.bytes(&v.to_le_bytes());
    }
    fn str(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.bytes(s.as_bytes());
    }
    fn flag(&mut self, b: bool) {
        self.bytes(&[u8::from(b)]);
    }
    pub(super) fn finish(&self) -> u64 {
        self.0
    }
}

pub(super) fn hash_str(s: &str) -> u64 {
    let mut h = Fnv::new();
    h.str(s);
    h.finish()
}

/// What a snapshot context depends on besides the templates: the init
/// script run in it and the base part of the realm fixups (the removals
/// are per realm and run on every restore).
pub(super) fn init_digest(init_script: &str, fixups: Option<&InterfaceFixups>) -> u64 {
    let mut h = Fnv::new();
    h.str(init_script);
    let mut ids: Vec<&InterfaceId> = fixups.map(|f| f.keys().collect()).unwrap_or_default();
    ids.sort_by_key(|id| id.0);
    for id in ids {
        let Some(fx) = fixups.and_then(|f| f.get(id)) else {
            continue;
        };
        let base = fx.base();
        if base.is_empty() {
            continue;
        }
        h.u32(id.0);
        for list in [&base.proto_late, &base.unscopables] {
            h.u32(list.len() as u32);
            for k in list {
                h.str(k);
            }
        }
        h.flag(base.error_proto);
    }
    h.finish()
}

fn hash_key(h: &mut Fnv, k: &PropKey) {
    match k {
        PropKey::Name(n) => {
            h.bytes(&[0]);
            h.str(n);
        }
        PropKey::Symbol(s) => h.bytes(&[1, *s as u8]),
    }
}

fn hash_member(h: &mut Fnv, m: &Member) {
    hash_key(h, &m.key);
    h.flag(m.attrs.writable);
    h.flag(m.attrs.enumerable);
    h.flag(m.attrs.configurable);
    match &m.kind {
        MemberKind::Method { length, .. } => {
            h.bytes(&[0]);
            h.u32(*length);
        }
        MemberKind::Accessor { get, set } => {
            h.bytes(&[1]);
            h.flag(get.is_some());
            h.flag(set.is_some());
        }
        MemberKind::Value(v) => {
            h.bytes(&[2]);
            match v {
                JsValue::Undefined => h.bytes(&[0]),
                JsValue::Null => h.bytes(&[1]),
                JsValue::Bool(b) => h.bytes(&[2, u8::from(*b)]),
                JsValue::Number(n) => {
                    h.bytes(&[3]);
                    h.bytes(&n.to_bits().to_le_bytes());
                }
                JsValue::String(s) | JsValue::Json(s) | JsValue::Opaque(s) => {
                    h.bytes(&[4]);
                    h.str(s);
                }
            }
        }
        MemberKind::Interface(id) => {
            h.bytes(&[3]);
            h.u32(id.0);
        }
        MemberKind::Intrinsic(i) => h.bytes(&[4, *i as u8]),
        MemberKind::Alias(of) => {
            h.bytes(&[5]);
            hash_key(h, of);
        }
        MemberKind::Namespace {
            to_string_tag,
            members,
        } => {
            h.bytes(&[6]);
            h.str(to_string_tag);
            h.u32(members.len() as u32);
            for m in members {
                hash_member(h, m);
            }
        }
    }
}

/// Everything about a spec that shapes its templates (not the closures).
pub(super) fn spec_digest(spec: &InterfaceSpec) -> u64 {
    let mut h = Fnv::new();
    h.str(&spec.name);
    h.u32(spec.parent.map_or(u32::MAX, |p| p.0));
    h.flag(spec.constructor.is_some());
    h.u32(spec.length);
    h.flag(spec.to_string_tag);
    h.flag(spec.immutable_proto);
    h.u32(spec.prototype_from.map_or(u32::MAX, |p| p.0));
    for list in [&spec.statics, &spec.prototype, &spec.instance] {
        h.u32(list.len() as u32);
        for m in list {
            hash_member(&mut h, m);
        }
    }
    h.flag(spec.named_getter.is_some());
    h.flag(spec.named_setter.is_some());
    h.flag(spec.named_deleter.is_some());
    h.flag(spec.named_enumerator.is_some());
    h.flag(spec.indexed_getter.is_some());
    h.flag(spec.indexed_length.is_some());
    h.flag(spec.named_unenumerable);
    h.u32(spec.reorder_last.len() as u32);
    for k in &spec.reorder_last {
        hash_key(&mut h, k);
    }
    h.u32(MAX_TEMPLATE_PROPS as u32);
    h.u32(OVERFLOW_CHUNK as u32);
    h.finish()
}

// ---------------------------------------------------------------------------
// Callback replay
// ---------------------------------------------------------------------------

/// Push the callbacks of `spec` in exactly the order
/// `Engine::define_interface` and `install_member` push them.
fn push_spec_callbacks(st: &mut State, spec: InterfaceSpec) {
    st.push_callback(Callback::Ctor {
        iface: spec.name.as_str().into(),
        f: spec.constructor,
    });
    for m in spec
        .statics
        .iter()
        .chain(&spec.prototype)
        .chain(&spec.instance)
    {
        push_member_callbacks(st, m);
    }
    if let Some(getter) = spec.named_getter {
        st.push_callback(Callback::Named(getter));
        st.push_callback(
            spec.named_setter
                .map_or(Callback::Empty, Callback::NamedSet),
        );
        st.push_callback(
            spec.named_deleter
                .map_or(Callback::Empty, Callback::NamedDelete),
        );
        st.push_callback(
            spec.named_enumerator
                .map_or(Callback::Empty, Callback::NamedEnum),
        );
    }
    if let Some(getter) = spec.indexed_getter {
        st.push_callback(Callback::Indexed(getter));
        if let Some(len) = spec.indexed_length {
            st.push_callback(Callback::IndexedLen(len));
        }
    }
}

fn push_member_callbacks(st: &mut State, m: &Member) {
    match &m.kind {
        MemberKind::Method { f, .. } => {
            st.push_callback(Callback::Fn(f.clone()));
        }
        MemberKind::Accessor { get, set } => {
            if let Some(f) = get {
                st.push_callback(Callback::Fn(f.clone()));
            }
            if let Some(f) = set {
                st.push_callback(Callback::Fn(f.clone()));
            }
        }
        MemberKind::Namespace { members, .. } => {
            for m in members {
                push_member_callbacks(st, m);
            }
        }
        MemberKind::Value(_)
        | MemberKind::Interface(_)
        | MemberKind::Intrinsic(_)
        | MemberKind::Alias(_) => {}
    }
}

/// Try to define `spec` from the snapshot. `None`: build it normally (no
/// snapshot, past its end, or diverged; divergence is permanent).
pub(super) fn restore_interface(st: &mut State, spec: &InterfaceSpec) -> Option<InterfaceId> {
    let id = st.interfaces.len();
    let cb_len = st.callbacks.len() as u32;
    let SnapState::Restoring(r) = &mut st.snap else {
        return None;
    };
    if r.diverged || r.next >= r.meta.digests.len() {
        return None;
    }
    let i = r.next;
    if id != i || r.meta.cb_before[i] != cb_len || r.meta.digests[i] != spec_digest(spec) {
        r.diverged = true;
        return None;
    }
    let (Some(template), Some(overflow)) = (r.templates[i].take(), r.overflows[i].take()) else {
        r.diverged = true;
        return None;
    };
    let expect_after = r.meta.cb_after[i] as usize;
    r.next += 1;
    let split = template_split(&spec.instance);
    let (items, plan) = overflow_plan(&spec.instance[split..]);
    if plan.len() != overflow.len() {
        r.diverged = true;
        return None;
    }
    // Keys and the `reorder_last` plan are made on first use (no scope
    // here; see `overflow_of`).
    let overflow = Rc::new(Overflow {
        chunks: overflow,
        items,
        direct: Vec::new(),
        keys: Vec::new(),
        deferred: Vec::new(),
        last: Vec::new(),
    });
    let parent = spec.parent;
    let prototype_from = spec.prototype_from;
    let reorder_last = spec.reorder_last.clone();
    push_spec_callbacks(st, spec.clone());
    // The replay mirrors the builder; a count mismatch is a bug in this
    // file, caught by the tests.
    debug_assert_eq!(
        st.callbacks.len(),
        expect_after,
        "snapshot replay of {}",
        spec.name
    );
    st.interfaces.push(IfaceSlot::Built(IfaceEntry {
        template,
        parent,
        prototype_from,
        overflow,
        reorder_last,
    }));
    Some(InterfaceId(id as u32))
}

/// Record a just-built interface in a creator's header.
pub(super) fn record_interface(st: &mut State, digest: u64, cb_before: u32) {
    let cb_after = st.callbacks.len() as u32;
    if let SnapState::Creating(meta) = &mut st.snap {
        meta.digests.push(digest);
        meta.cb_before.push(cb_before);
        meta.cb_after.push(cb_after);
    }
}

// ---------------------------------------------------------------------------
// Engine entry points
// ---------------------------------------------------------------------------

impl Engine {
    /// An engine that records a startup snapshot: define the interfaces as
    /// usual, then call [`Engine::create_snapshot`]. Use a persona-free
    /// setup (a default persona); persona values that reach templates make
    /// the snapshot diverge for other personas.
    ///
    /// Only one creator per process ([`EngineError::SnapshotCreatorUsed`]),
    /// and [`Engine::create_snapshot`] fails while any other engine is
    /// alive: V8 writes into the read-only heap all isolates share. Make the
    /// snapshot before the first other engine (as `lb_webapi::snapshot`
    /// does).
    pub fn snapshot_creator(persona: Arc<Persona>) -> Result<Engine, EngineError> {
        static USED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if USED.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return Err(EngineError::SnapshotCreatorUsed);
        }
        Engine::build(persona, EngineOptions::default(), Build::Creator)
    }

    /// An engine restored from a blob made by [`Engine::create_snapshot`]
    /// with the same binary. Define the same interfaces in the same order
    /// (their templates come from the blob), then call
    /// [`Engine::use_snapshot_context`]. A blob from another V8 version or
    /// with other V8 flags is rejected and the engine builds normally (see
    /// [`Engine::snapshot_status`]).
    pub fn with_snapshot(
        persona: Arc<Persona>,
        options: EngineOptions,
        blob: &'static [u8],
    ) -> Result<Engine, EngineError> {
        Engine::build(persona, options, Build::Restore(blob))
    }

    /// The snapshot state of this engine.
    pub fn snapshot_status(&self) -> SnapshotStatus {
        self.state.borrow().snap.status()
    }

    /// After all interfaces are defined: let new realms whose global is
    /// `global` be deserialized from the snapshot context, if every
    /// interface was replayed, `global` is the snapshot's window and
    /// `init_script` is the script the snapshot ran. Such realms skip the
    /// init script (see [`RealmOps::is_snapshot_realm`]). Returns whether
    /// the snapshot context is in use.
    pub fn use_snapshot_context(
        &mut self,
        global: InterfaceId,
        init_script: &str,
        fixups: Option<&InterfaceFixups>,
    ) -> bool {
        let mut st = self.state.borrow_mut();
        let SnapState::Restoring(r) = &mut st.snap else {
            return false;
        };
        r.context_ready = !r.diverged
            && r.next == r.meta.digests.len()
            && global.0 == r.meta.window
            && init_digest(init_script, fixups) == r.meta.init_hash;
        r.context_ready
    }

    /// Serialize a creator engine: the interface templates plus one context
    /// whose global is `global`, set up like [`Engine::new_realm`] and with
    /// `init_script` run in it. Consumes the engine. The blob lives for the
    /// rest of the process and becomes its snapshot: every engine made
    /// afterwards comes from it (V8 shares one read-only heap between all
    /// isolates), plain ones without replay.
    pub fn create_snapshot(
        self,
        global: InterfaceId,
        init_script: &str,
        fixups: Option<InterfaceFixups>,
    ) -> Result<&'static [u8], JsError> {
        if !matches!(self.state.borrow().snap, SnapState::Creating(_)) {
            return Err(JsError::host("not a snapshot creator engine"));
        }
        let mut this = std::mem::ManuallyDrop::new(self);
        // SAFETY: each field is moved out exactly once and `Engine::drop`
        // never runs for `this`; the isolate is consumed by `create_blob`
        // (V8's SnapshotCreator owns and disposes it) or leaked.
        let (mut isolate, state, persona, setup, live) = unsafe {
            (
                std::mem::ManuallyDrop::take(&mut this.isolate),
                std::ptr::read(&this.state),
                std::ptr::read(&this.persona),
                this.setup.take(),
                std::ptr::read(&this.live),
            )
        };
        drop(persona);
        // SAFETY: the SnapshotCreator exits and disposes the isolate once
        // when the blob is created, so it must be entered (once) then.
        unsafe { isolate.enter() };
        let result = fill_snapshot(&mut isolate, &state, global, init_script, fixups);
        // Every `Global` must be gone before serializing.
        {
            let mut st = state.borrow_mut();
            st.wrappers.clear();
            st.handles.clear();
            st.resolvers.clear();
            st.realms.clear();
            st.interfaces.clear();
            st.callbacks.clear();
            st.creating = None;
        }
        isolate.remove_slot::<SharedState>();
        drop(state);
        // `CreateBlob` writes into the read-only heap all isolates share;
        // with another isolate alive it crashes. No new one can appear
        // meanwhile: this creator holds `ISOLATE_SETUP`.
        let alone = setup.as_ref().is_some_and(|s| s.live == 1);
        let result = result.and_then(|meta| {
            alone.then_some(meta).ok_or_else(|| {
                JsError::host("a snapshot can only be made while no other engine exists")
            })
        });
        let (meta, blob) = match result {
            Ok(meta) => (meta, isolate.create_blob(v8::FunctionCodeHandling::Clear)),
            Err(e) => {
                // V8 only lets a creator go through `CreateBlob`: leak it,
                // and keep counting it as alive.
                // SAFETY: balances the enter above; never used again.
                unsafe { isolate.exit() };
                std::mem::forget(isolate);
                std::mem::forget(live);
                drop(setup);
                return Err(e);
            }
        };
        let result = blob
            .ok_or_else(|| JsError::host("V8 could not create the snapshot"))
            .map(|blob| {
                let mut out = Vec::with_capacity(blob.len() + 64 * 1024);
                meta.encode(&mut out);
                out.extend_from_slice(&blob);
                let out: &'static [u8] = Box::leak(out.into_boxed_slice());
                out
            });
        let mut setup = setup;
        if let (Ok(blob), Some(s)) = (&result, setup.as_mut()) {
            s.blob = Some(blob);
        }
        drop(setup);
        drop(live);
        result
    }
}

/// Build the snapshot context and register templates as isolate data.
fn fill_snapshot(
    isolate: &mut v8::OwnedIsolate,
    state: &SharedState,
    global: InterfaceId,
    init_script: &str,
    fixups: Option<InterfaceFixups>,
) -> Result<Meta, JsError> {
    let mut meta = match &state.borrow().snap {
        SnapState::Creating(m) => m.clone(),
        _ => unreachable!("checked by create_snapshot"),
    };
    let n = state.borrow().interfaces.len();
    if meta.digests.len() != n {
        return Err(JsError::host(
            "interfaces were defined outside the snapshot recorder",
        ));
    }
    if global.0 as usize >= n {
        return Err(JsError::host(format!("unknown interface {global:?}")));
    }
    meta.window = global.0;
    meta.init_hash = init_digest(init_script, fixups.as_ref());
    meta.v8_version = v8::V8::get_version().to_owned();
    meta.v8_flags = v8_flags();

    v8::scope!(let scope, isolate);
    // Default context: what `Context::new` without a template starts from.
    let default = v8::Context::new(scope, Default::default());
    scope.set_default_context(default);

    // Set up like a realm's (`new_realm_in`), but registered as no realm:
    // a realm id would put an identity hash on the global, shared by every
    // realm made from it. Interface objects stay lazy except the ones V8
    // makes with the global; base fixups only (removals are per realm).
    let context = new_context(scope, state, Some(global));
    let base: Option<InterfaceFixups> = fixups.map(|f| {
        Rc::new(
            f.iter()
                .map(|(id, fx)| (*id, fx.base()))
                .filter(|(_, fx)| !fx.is_empty())
                .collect(),
        )
    });
    let mat = Rc::new(RealmMat {
        made: RefCell::new(Vec::new()),
        fixups: base,
    });
    state.borrow_mut().creating = Some(mat.clone());
    let setup = {
        let cs = &mut v8::ContextScope::new(scope, context);
        let g = context.global(cs);
        setup_global(cs, state, &mat, global, g).and_then(|()| {
            v8::tc_scope!(let tc, cs);
            match run_script(tc, init_script, ScriptOrigin::new("")) {
                Some(_) => Ok(()),
                None => Err(caught_nested(tc)),
            }
        })
    };
    state.borrow_mut().creating = None;
    setup?;
    meta.made = mat
        .made
        .borrow()
        .iter()
        .enumerate()
        .filter(|(_, m)| **m)
        .map(|(i, _)| i as u32)
        .collect();
    let index = scope.add_context(context);
    if index != 0 {
        return Err(JsError::host("snapshot context is not the first one"));
    }
    let st = state.borrow();
    for (i, slot) in st.interfaces.iter().enumerate() {
        let entry = slot
            .built()
            .ok_or_else(|| JsError::host("lazy interface in a snapshot creator"))?;
        let t = v8::Local::new(scope, &entry.template);
        let at = scope.add_isolate_data(t);
        if at != i {
            return Err(JsError::host("unexpected snapshot data index"));
        }
    }
    for entry in st.interfaces.iter().filter_map(IfaceSlot::built) {
        let mut first = u32::MAX;
        for (j, ot) in entry.overflow.chunks.iter().enumerate() {
            let t = v8::Local::new(scope, ot);
            let at = scope.add_isolate_data(t) as u32;
            if j == 0 {
                first = at;
            } else if at != first + j as u32 {
                return Err(JsError::host("unexpected snapshot data index"));
            }
        }
        meta.overflow_index.push(first);
        meta.overflow_count.push(entry.overflow.chunks.len() as u32);
    }
    Ok(meta)
}

/// Load the templates of a restoring isolate. Called once at creation.
pub(super) fn load_templates(
    isolate: &mut v8::OwnedIsolate,
    meta: Meta,
) -> Result<Box<Restore>, String> {
    v8::scope!(let scope, isolate);
    // Isolate data is read through a context scope.
    let context = v8::Context::new(scope, Default::default());
    let scope = &mut v8::ContextScope::new(scope, context);
    let n = meta.digests.len();
    let mut templates = Vec::with_capacity(n);
    for i in 0..n {
        let t = scope
            .get_isolate_data_from_snapshot_once::<v8::FunctionTemplate>(i)
            .map_err(|e| format!("snapshot template {i}: {e}"))?;
        templates.push(Some(v8::Global::new(scope, t)));
    }
    let mut overflows = Vec::with_capacity(n);
    for (&first, &count) in meta.overflow_index.iter().zip(&meta.overflow_count) {
        let mut chunks = Vec::with_capacity(count as usize);
        for at in (0..count).map(|j| first as usize + j as usize) {
            let t = scope
                .get_isolate_data_from_snapshot_once::<v8::ObjectTemplate>(at)
                .map_err(|e| format!("snapshot overflow template {at}: {e}"))?;
            chunks.push(v8::Global::new(scope, t));
        }
        overflows.push(Some(chunks));
    }
    Ok(Box::new(Restore {
        meta,
        templates,
        overflows,
        next: 0,
        diverged: false,
        context_ready: false,
    }))
}

/// Check a blob before giving it to V8; returns its header and V8 part.
pub(super) fn check_blob(blob: &'static [u8]) -> Result<(Meta, &'static [u8]), String> {
    let (meta, v8_part) = Meta::decode(blob)?;
    if meta.v8_version != v8::V8::get_version() {
        return Err(format!(
            "snapshot is for V8 {}, this is {}",
            meta.v8_version,
            v8::V8::get_version()
        ));
    }
    if meta.v8_flags != v8_flags() {
        return Err("snapshot was created with other V8 flags".into());
    }
    if meta.overflow_index.len() != meta.digests.len()
        || meta.overflow_count.len() != meta.digests.len()
    {
        return Err("snapshot header is inconsistent".into());
    }
    if !v8::StartupData::from(v8_part).is_valid() {
        return Err("V8 rejected the snapshot data".into());
    }
    Ok((meta, v8_part))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_round_trip() {
        let m = Meta {
            v8_version: "1.2.3".into(),
            v8_flags: "--jitless".into(),
            window: 7,
            init_hash: 42,
            made: vec![3, 7],
            digests: vec![1, 2],
            cb_before: vec![0, 3],
            cb_after: vec![3, 9],
            overflow_index: vec![u32::MAX, 2],
            overflow_count: vec![0, 3],
        };
        let mut out = Vec::new();
        m.encode(&mut out);
        assert_eq!(out.len() % 8, 0);
        out.extend_from_slice(b"v8data");
        let (back, rest) = Meta::decode(&out).unwrap();
        assert_eq!(back, m);
        assert_eq!(rest, b"v8data");
        assert!(Meta::decode(b"LBSNAP00").is_err());
    }

    #[test]
    fn digest_sees_values_and_shape() {
        let a =
            InterfaceSpec::new("A").prototype(vec![Member::constant("X", JsValue::Number(1.0))]);
        let b =
            InterfaceSpec::new("A").prototype(vec![Member::constant("X", JsValue::Number(2.0))]);
        let c = InterfaceSpec::new("A").instance(vec![Member::constant("X", JsValue::Number(1.0))]);
        assert_ne!(spec_digest(&a), spec_digest(&b));
        assert_ne!(spec_digest(&a), spec_digest(&c));
        assert_eq!(spec_digest(&a), spec_digest(&a.clone()));
    }
}

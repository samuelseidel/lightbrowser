//! V8 backend (feature `chrome`).
//!
//! All host state lives in one `Rc<RefCell<State>>` stored both in [`Engine`]
//! and in an isolate slot, so V8 callbacks can reach it. Borrows of it are
//! always short and never held while JS runs, so callbacks may re-enter JS.
//!
//! Every host callback goes through one trampoline per callback shape; the
//! template's data is a small integer index into `State::callbacks`. That
//! keeps closures out of V8 and keeps the external-reference list short for
//! the M6 startup snapshot.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::num::NonZeroI32;
use std::rc::Rc;
use std::sync::{Arc, Once};

use lb_persona::{BrowserFamily, Persona};

pub use v8;

#[doc(hidden)]
pub mod icu_data;
mod inspect;
mod realm_ops;
mod snapshot;
#[doc(hidden)]
pub mod unused_v8;
pub use inspect::{Caught, Inspected, InspectedObject, PromiseOutcome, PropertyEntry};
pub use realm_ops::RealmOps;
pub use snapshot::SnapshotStatus;
use snapshot::{Meta, SnapState};

use crate::{
    EngineError, HeapStats, HostRef, InterfaceFixup, InterfaceFixups, InterfaceId, JsEngine,
    JsError, JsErrorKind, JsValue, ModuleLoader, PropAttrs, Realm, RealmId, RealmOptions,
    ScriptOrigin, ValueKind, World,
};

// ---------------------------------------------------------------------------
// Process-wide init
// ---------------------------------------------------------------------------

static V8_INIT: Once = Once::new();
/// The flags V8 was initialised with (a startup snapshot must match them).
static V8_FLAGS: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Initialise the V8 platform once per process. `flags` (e.g. `--jitless`)
/// only apply if this is the first call; [`Engine::new`] calls it with no
/// flags. Returns whether this call did the initialisation.
///
/// Panics on a flag that would reach V8 code the binary does not link
/// ([`unused_v8::REFUSED_FLAGS`], ADR 0013).
pub fn init_v8(flags: &str) -> bool {
    let mut did = false;
    let refused = unused_v8::refused_flags(flags);
    assert!(
        refused.is_empty(),
        "init_v8: {refused:?} would reach V8 code that is not linked (lb-js unused_v8)"
    );
    V8_INIT.call_once(|| {
        let _ = V8_FLAGS.set(flags.to_owned());
        if !flags.is_empty() {
            v8::V8::set_flags_from_string(flags);
        }
        // ICU's data is not linked in; register our copy first (ADR 0013).
        icu_data::install();
        let platform = v8::new_default_platform(0, false).make_shared();
        v8::V8::initialize_platform(platform);
        v8::V8::initialize();
        did = true;
    });
    did
}

/// Which snapshot the isolates of the process are made from.
///
/// V8 keeps one read-only heap per process, shared by every isolate. So
/// isolates alive at the same time must all come from the same snapshot
/// (V8's built-in one, or one blob of ours), and
/// `SnapshotCreator::CreateBlob` (which writes into that heap) needs to be
/// alone. Once the process has a blob, every isolate is made from it.
/// Held while an isolate is created, and for a snapshot creator's life.
struct IsolateSetup {
    /// Isolates alive (decremented after disposal).
    live: usize,
    /// The process's snapshot blob, once made or first restored.
    blob: Option<&'static [u8]>,
}

static ISOLATE_SETUP: std::sync::Mutex<IsolateSetup> = std::sync::Mutex::new(IsolateSetup {
    live: 0,
    blob: None,
});

/// Lock [`ISOLATE_SETUP`]; a panic while it was held does not matter (the
/// counts are updated in single steps).
fn isolate_setup_lock() -> std::sync::MutexGuard<'static, IsolateSetup> {
    ISOLATE_SETUP
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Counts one isolate in [`IsolateSetup::live`] while it exists.
struct LiveIsolate;

impl LiveIsolate {
    fn new(setup: &mut IsolateSetup) -> Self {
        setup.live += 1;
        LiveIsolate
    }
}

impl Drop for LiveIsolate {
    fn drop(&mut self) {
        isolate_setup_lock().live -= 1;
    }
}

/// Flags passed to [`init_v8`] (empty before initialisation).
fn v8_flags() -> String {
    V8_FLAGS.get().cloned().unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Public host-facing types
// ---------------------------------------------------------------------------

/// A Rust function exposed to JS (operation, getter, setter, constructor).
pub type HostFn = Rc<dyn Fn(&mut CallCtx<'_, '_, '_>) -> Result<(), HostError>>;
/// Named property getter (`storage.foo`, `window.someId`). `Ok(None)` means
/// "not handled": normal lookup continues.
pub type NamedGetter = Rc<dyn Fn(&mut PropCtx<'_, '_, '_>, &str) -> Result<Option<Arg>, HostError>>;
/// Indexed property getter (`nodeList[0]`).
pub type IndexedGetter =
    Rc<dyn Fn(&mut PropCtx<'_, '_, '_>, u32) -> Result<Option<Arg>, HostError>>;
/// Named property setter (`storage.foo = v`, `style.color = v`). The value
/// is passed after ToString (every WebIDL named setter Chrome exposes on
/// the window takes a `DOMString`). `Ok(false)` means "not handled": the
/// assignment proceeds normally (prototype accessors, own data property).
pub type NamedSetter = Rc<dyn Fn(&mut PropCtx<'_, '_, '_>, &str, &str) -> Result<bool, HostError>>;
/// Named property deleter (`delete storage.foo`). `Ok(None)` means "not
/// handled"; `Ok(Some(b))` makes `delete` return `b`.
pub type NamedDeleter =
    Rc<dyn Fn(&mut PropCtx<'_, '_, '_>, &str) -> Result<Option<bool>, HostError>>;
/// Names the named getter supports, for `Object.keys` / `for in`.
pub type NamedEnumerator = Rc<dyn Fn(&mut PropCtx<'_, '_, '_>) -> Vec<String>>;
/// Number of supported property indices (`list.length`). With it, indexed
/// properties enumerate (`Object.keys`, `for in`, `JSON.stringify`) and
/// have Blink's read-only descriptor; assigning or deleting them is a
/// no-op (`delete` returns false).
pub type IndexedLength = Rc<dyn Fn(&mut PropCtx<'_, '_, '_>) -> u32>;

/// Box a closure as a [`HostFn`].
pub fn host_fn(f: impl Fn(&mut CallCtx<'_, '_, '_>) -> Result<(), HostError> + 'static) -> HostFn {
    Rc::new(f)
}

/// A persistent reference to a JS value, owned by the host. Copyable; a
/// released or dropped-realm handle is detected and reported as an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Handle {
    idx: u32,
    generation: u32,
}

/// Resolve/reject capability of a promise created from Rust.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Resolver {
    idx: u32,
    generation: u32,
}

/// A value passed from Rust into JS.
#[derive(Debug, Clone, PartialEq)]
pub enum Arg {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    /// Parsed with `JSON.parse` in the target realm.
    Json(String),
    /// A value held by the host.
    Handle(Handle),
    /// The wrapper of a host object as an instance of the interface.
    Host(InterfaceId, HostRef),
    /// A new object made by `new X(...args)`, where `X` is the interface
    /// object of the current realm (e.g. a `DOMException` to throw).
    Construct(InterfaceId, Vec<Arg>),
    /// A new JS array of these values (`sequence<T>` results).
    Array(Vec<Arg>),
    /// A new `ArrayBuffer` holding these bytes.
    ArrayBuffer(Vec<u8>),
    /// A new `Uint8Array` (over a fresh `ArrayBuffer`) holding these bytes.
    Uint8Array(Vec<u8>),
    /// A new `Uint8ClampedArray` (over a fresh `ArrayBuffer`), e.g.
    /// `ImageData.data`.
    Uint8ClampedArray(Vec<u8>),
    /// A new `Float32Array` (WebGL parameters, Web Audio channel data).
    Float32Array(Vec<f32>),
    /// A new `Int32Array` (WebGL parameters such as `VIEWPORT`).
    Int32Array(Vec<i32>),
    /// A new `Uint32Array` (WebGL parameters such as
    /// `COMPRESSED_TEXTURE_FORMATS`).
    Uint32Array(Vec<u32>),
}

impl From<JsValue> for Arg {
    fn from(v: JsValue) -> Self {
        match v {
            JsValue::Undefined => Arg::Undefined,
            JsValue::Null => Arg::Null,
            JsValue::Bool(b) => Arg::Bool(b),
            JsValue::Number(n) => Arg::Number(n),
            JsValue::String(s) | JsValue::Opaque(s) => Arg::String(s),
            JsValue::Json(s) => Arg::Json(s),
        }
    }
}
impl From<bool> for Arg {
    fn from(v: bool) -> Self {
        Arg::Bool(v)
    }
}
impl From<f64> for Arg {
    fn from(v: f64) -> Self {
        Arg::Number(v)
    }
}
impl From<i32> for Arg {
    fn from(v: i32) -> Self {
        Arg::Number(v.into())
    }
}
impl From<u32> for Arg {
    fn from(v: u32) -> Self {
        Arg::Number(v.into())
    }
}
impl From<&str> for Arg {
    fn from(v: &str) -> Self {
        Arg::String(v.to_owned())
    }
}
impl From<String> for Arg {
    fn from(v: String) -> Self {
        Arg::String(v)
    }
}
impl From<Handle> for Arg {
    fn from(v: Handle) -> Self {
        Arg::Handle(v)
    }
}
impl From<()> for Arg {
    fn from(_: ()) -> Self {
        Arg::Undefined
    }
}

/// An exception thrown by a host callback.
#[derive(Debug, Clone, PartialEq)]
pub enum HostError {
    TypeError(String),
    RangeError(String),
    SyntaxError(String),
    ReferenceError(String),
    Error(String),
    /// Throw this value as is (e.g. a `DOMException` wrapper).
    Throw(Arg),
    /// A JS exception is already pending (from a conversion or a nested
    /// call); let it propagate.
    Thrown,
}

impl HostError {
    pub fn type_error(msg: impl Into<String>) -> Self {
        HostError::TypeError(msg.into())
    }
    pub fn range_error(msg: impl Into<String>) -> Self {
        HostError::RangeError(msg.into())
    }
}

/// Well-known symbols usable as property keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WellKnownSymbol {
    Iterator,
    AsyncIterator,
    ToStringTag,
    ToPrimitive,
    Unscopables,
}

/// A property key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PropKey {
    Name(String),
    Symbol(WellKnownSymbol),
}

impl From<&str> for PropKey {
    fn from(v: &str) -> Self {
        PropKey::Name(v.to_owned())
    }
}
impl From<String> for PropKey {
    fn from(v: String) -> Self {
        PropKey::Name(v)
    }
}
impl From<WellKnownSymbol> for PropKey {
    fn from(v: WellKnownSymbol) -> Self {
        PropKey::Symbol(v)
    }
}

/// Built-ins that WebIDL reuses by identity, e.g. iterable `values` is
/// `Array.prototype.values` of the same realm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intrinsic {
    ArrayProtoEntries,
    ArrayProtoForEach,
    ArrayProtoKeys,
    ArrayProtoValues,
}

/// What a member is.
#[derive(Clone)]
pub enum MemberKind {
    /// A function backed by Rust. `length` is the WebIDL required-argument
    /// count.
    Method { length: u32, f: HostFn },
    /// Accessor property with optional getter/setter functions.
    Accessor {
        get: Option<HostFn>,
        set: Option<HostFn>,
    },
    /// A primitive data property (constants). `Json`/`Opaque` are stored as
    /// strings.
    Value(JsValue),
    /// The interface object of another interface (global members).
    Interface(InterfaceId),
    /// A built-in of the realm.
    Intrinsic(Intrinsic),
    /// The same function object as an earlier [`MemberKind::Method`] of the
    /// same member list (WebIDL `[Symbol.iterator]` is `entries`).
    Alias(PropKey),
    /// A plain object (instantiated per realm) with these members and a
    /// `Symbol.toStringTag` (WebIDL namespaces such as `CSS`).
    Namespace {
        to_string_tag: String,
        members: Vec<Member>,
    },
}

/// One property of an interface object, prototype or instance.
#[derive(Clone)]
pub struct Member {
    pub key: PropKey,
    pub kind: MemberKind,
    pub attrs: PropAttrs,
}

impl Member {
    pub fn method(key: impl Into<PropKey>, length: u32, f: HostFn) -> Self {
        Member {
            key: key.into(),
            kind: MemberKind::Method { length, f },
            attrs: PropAttrs::DEFAULT,
        }
    }
    pub fn getter(key: impl Into<PropKey>, get: HostFn) -> Self {
        Self::accessor(key, Some(get), None)
    }
    pub fn accessor(key: impl Into<PropKey>, get: Option<HostFn>, set: Option<HostFn>) -> Self {
        Member {
            key: key.into(),
            kind: MemberKind::Accessor { get, set },
            attrs: PropAttrs::DEFAULT,
        }
    }
    /// A WebIDL constant (read-only, enumerable, non-configurable).
    pub fn constant(key: impl Into<PropKey>, value: JsValue) -> Self {
        Member {
            key: key.into(),
            kind: MemberKind::Value(value),
            attrs: PropAttrs::CONSTANT,
        }
    }
    /// A plain data property with default attributes.
    pub fn value(key: impl Into<PropKey>, value: JsValue) -> Self {
        Member {
            key: key.into(),
            kind: MemberKind::Value(value),
            attrs: PropAttrs::DEFAULT,
        }
    }
    /// An interface object as a global property (not enumerable).
    pub fn interface(key: impl Into<PropKey>, id: InterfaceId) -> Self {
        Member {
            key: key.into(),
            kind: MemberKind::Interface(id),
            attrs: PropAttrs::HIDDEN,
        }
    }
    pub fn intrinsic(key: impl Into<PropKey>, which: Intrinsic) -> Self {
        Member {
            key: key.into(),
            kind: MemberKind::Intrinsic(which),
            attrs: PropAttrs::DEFAULT,
        }
    }
    /// Same function object as the earlier method `of` in the same list.
    pub fn alias(key: impl Into<PropKey>, of: impl Into<PropKey>) -> Self {
        Member {
            key: key.into(),
            kind: MemberKind::Alias(of.into()),
            attrs: PropAttrs::DEFAULT,
        }
    }
    /// A namespace object (`CSS`) as a global property: writable,
    /// configurable, not enumerable.
    pub fn namespace(
        key: impl Into<PropKey>,
        to_string_tag: impl Into<String>,
        members: Vec<Member>,
    ) -> Self {
        Member {
            key: key.into(),
            kind: MemberKind::Namespace {
                to_string_tag: to_string_tag.into(),
                members,
            },
            attrs: PropAttrs::HIDDEN,
        }
    }
    /// Replace the attributes.
    pub fn with_attrs(mut self, attrs: PropAttrs) -> Self {
        self.attrs = attrs;
        self
    }
}

/// A WebIDL interface. Members are installed in list order, which becomes
/// the property order JS sees.
#[derive(Clone)]
pub struct InterfaceSpec {
    pub name: String,
    /// Must be defined on the same engine before this interface.
    pub parent: Option<InterfaceId>,
    /// `new X()` handler. `None` throws `TypeError: Illegal constructor`.
    pub constructor: Option<HostFn>,
    /// `X.length`.
    pub length: u32,
    /// Own properties of the interface object (constants, static ops).
    pub statics: Vec<Member>,
    /// Properties of `X.prototype`.
    pub prototype: Vec<Member>,
    /// Own properties of every instance; for the global interface, the
    /// global's own properties.
    pub instance: Vec<Member>,
    /// Add `X.prototype[Symbol.toStringTag] = "X"` (default true).
    pub to_string_tag: bool,
    /// Make instances immutable-prototype exotic objects (the global).
    pub immutable_proto: bool,
    /// A `[LegacyFactoryFunction]` (`Image`): `X.prototype` is this
    /// interface's prototype object (read-only, not enumerable, not
    /// configurable), as in Chrome, so `Image.prototype ===
    /// HTMLImageElement.prototype`. Its constructor should return with
    /// [`CallCtx::ret_constructed`].
    pub prototype_from: Option<InterfaceId>,
    pub named_getter: Option<NamedGetter>,
    pub indexed_getter: Option<IndexedGetter>,
    pub named_setter: Option<NamedSetter>,
    pub named_deleter: Option<NamedDeleter>,
    pub named_enumerator: Option<NamedEnumerator>,
    /// See [`IndexedLength`]; needs an indexed getter.
    pub indexed_length: Option<IndexedLength>,
    /// `[LegacyUnenumerableNamedProperties]`: named properties are
    /// read-only and not enumerable (`Object.keys` skips them,
    /// `getOwnPropertyNames` lists them). Needs a named getter.
    pub named_unenumerable: bool,
    /// For a global interface: own properties of the global (its
    /// `instance` members or the engine's built-ins) moved to the end, in
    /// this order, once the members are defined, in every realm
    /// (including ones from the snapshot, where V8 appends its flag-gated
    /// built-ins again). Interface objects among them stay lazy.
    pub reorder_last: Vec<PropKey>,
}

impl InterfaceSpec {
    pub fn new(name: impl Into<String>) -> Self {
        InterfaceSpec {
            name: name.into(),
            parent: None,
            constructor: None,
            length: 0,
            statics: Vec::new(),
            prototype: Vec::new(),
            instance: Vec::new(),
            to_string_tag: true,
            immutable_proto: false,
            prototype_from: None,
            named_getter: None,
            indexed_getter: None,
            named_setter: None,
            named_deleter: None,
            named_enumerator: None,
            indexed_length: None,
            named_unenumerable: false,
            reorder_last: Vec::new(),
        }
    }
    pub fn parent(mut self, parent: InterfaceId) -> Self {
        self.parent = Some(parent);
        self
    }
    pub fn constructor(mut self, length: u32, f: HostFn) -> Self {
        self.length = length;
        self.constructor = Some(f);
        self
    }
    pub fn statics(mut self, members: Vec<Member>) -> Self {
        self.statics = members;
        self
    }
    pub fn prototype(mut self, members: Vec<Member>) -> Self {
        self.prototype = members;
        self
    }
    pub fn instance(mut self, members: Vec<Member>) -> Self {
        self.instance = members;
        self
    }
    pub fn named_getter(mut self, f: NamedGetter) -> Self {
        self.named_getter = Some(f);
        self
    }
    pub fn indexed_getter(mut self, f: IndexedGetter) -> Self {
        self.indexed_getter = Some(f);
        self
    }
    /// Needs a named getter too; ignored without one.
    pub fn named_setter(mut self, f: NamedSetter) -> Self {
        self.named_setter = Some(f);
        self
    }
    /// Needs a named getter too; ignored without one.
    pub fn named_deleter(mut self, f: NamedDeleter) -> Self {
        self.named_deleter = Some(f);
        self
    }
    /// Needs a named getter too; ignored without one.
    pub fn named_enumerator(mut self, f: NamedEnumerator) -> Self {
        self.named_enumerator = Some(f);
        self
    }
    /// Needs an indexed getter too; ignored without one.
    pub fn indexed_length(mut self, f: IndexedLength) -> Self {
        self.indexed_length = Some(f);
        self
    }
}

/// Options for [`Engine::new`].
#[derive(Debug, Clone, Default)]
pub struct EngineOptions {
    /// Hard cap for the JS heap. Near it, the running script is terminated
    /// with [`JsErrorKind::OutOfMemory`]. `None` uses V8's default.
    pub max_heap_bytes: Option<usize>,
}

/// Stops running JS on an engine from any thread (script timeouts).
#[derive(Debug, Clone)]
pub struct TerminateHandle(v8::IsolateHandle);

impl TerminateHandle {
    /// Terminate the script running now (or the next one). Returns false if
    /// the engine is gone.
    pub fn terminate(&self) -> bool {
        self.0.terminate_execution()
    }
}

// ---------------------------------------------------------------------------
// Internal state
// ---------------------------------------------------------------------------

/// Generational slab: stale ids are detected instead of aliasing new
/// entries.
struct Slab<T> {
    entries: Vec<(u32, Option<T>)>,
    free: Vec<u32>,
}

impl<T> Slab<T> {
    fn new() -> Self {
        Slab {
            entries: Vec::new(),
            free: Vec::new(),
        }
    }
    fn insert(&mut self, value: T) -> (u32, u32) {
        if let Some(idx) = self.free.pop() {
            let entry = &mut self.entries[idx as usize];
            entry.0 = entry.0.wrapping_add(1);
            entry.1 = Some(value);
            (idx, entry.0)
        } else {
            self.entries.push((0, Some(value)));
            ((self.entries.len() - 1) as u32, 0)
        }
    }
    fn get(&self, idx: u32, generation: u32) -> Option<&T> {
        match self.entries.get(idx as usize) {
            Some((g, Some(v))) if *g == generation => Some(v),
            _ => None,
        }
    }
    fn remove(&mut self, idx: u32, generation: u32) -> Option<T> {
        match self.entries.get_mut(idx as usize) {
            Some((g, slot @ Some(_))) if *g == generation => {
                self.free.push(idx);
                slot.take()
            }
            _ => None,
        }
    }
    fn retain(&mut self, mut keep: impl FnMut(&T) -> bool) {
        for (idx, (_, slot)) in self.entries.iter_mut().enumerate() {
            if slot.as_ref().is_some_and(|v| !keep(v)) {
                *slot = None;
                self.free.push(idx as u32);
            }
        }
    }
    fn clear(&mut self) {
        self.retain(|_| false);
    }
}

enum Callback {
    Fn(HostFn),
    Ctor {
        iface: Rc<str>,
        f: Option<HostFn>,
    },
    Named(NamedGetter),
    Indexed(IndexedGetter),
    NamedSet(NamedSetter),
    NamedDelete(NamedDeleter),
    NamedEnum(NamedEnumerator),
    IndexedLen(IndexedLength),
    /// Placeholder keeping named callbacks at consecutive indexes.
    Empty,
}

/// Builds an interface's spec when its templates are first needed (see
/// [`Engine::define_interface_lazy`]).
pub type LazySpec = Box<dyn FnOnce() -> InterfaceSpec>;

/// An interface of the engine: templates made, or still to make.
enum IfaceSlot {
    Built(IfaceEntry),
    Pending(LazySpec),
    /// Being built (its parent first).
    Building,
}

impl IfaceSlot {
    fn built(&self) -> Option<&IfaceEntry> {
        match self {
            IfaceSlot::Built(e) => Some(e),
            _ => None,
        }
    }
}

struct IfaceEntry {
    template: v8::Global<v8::FunctionTemplate>,
    parent: Option<InterfaceId>,
    /// See [`InterfaceSpec::prototype_from`].
    prototype_from: Option<InterfaceId>,
    /// Instance members not on the instance template, defined on each
    /// instance after it is created, in order (see `apply_overflow`).
    overflow: Overflow,
    /// See [`InterfaceSpec::reorder_last`].
    reorder_last: Vec<PropKey>,
}

/// Instance members defined after creation: interface objects directly
/// from their function templates, the others copied from chunk templates
/// of at most [`OVERFLOW_CHUNK`]. Direct definition also keeps each
/// interface object's own name: V8 names a template function after the
/// first property it is instantiated for, and a `[LegacyWindowAlias]`
/// (`webkitURL`) comes before its interface (`URL`) on the global; as in
/// Chrome, `webkitURL.name` is "URL".
#[derive(Default)]
struct Overflow {
    chunks: Vec<v8::Global<v8::ObjectTemplate>>,
    items: Vec<OverflowItem>,
    /// Per item, when the engine built the templates itself (not restored
    /// from a snapshot): the member's function templates or value, so it
    /// is defined directly instead of copied from a chunk instance. Same
    /// functions (V8 caches one per template and context), same
    /// descriptor.
    direct: Vec<Option<Direct>>,
}

/// A chunk member defined directly (see [`Overflow::direct`]).
enum Direct {
    Method(v8::Global<v8::FunctionTemplate>, PropAttrs),
    Accessor(
        Option<v8::Global<v8::FunctionTemplate>>,
        Option<v8::Global<v8::FunctionTemplate>>,
        PropAttrs,
    ),
    Value(JsValue, PropAttrs),
}

/// What [`install_member`] made, for [`Direct`].
enum Made<'s> {
    Method(v8::Local<'s, v8::FunctionTemplate>),
    Accessor(
        Option<v8::Local<'s, v8::FunctionTemplate>>,
        Option<v8::Local<'s, v8::FunctionTemplate>>,
    ),
    Value,
}

#[derive(Clone)]
enum OverflowItem {
    Interface {
        key: PropKey,
        id: InterfaceId,
        attrs: PropAttrs,
    },
    Chunk {
        key: PropKey,
        chunk: usize,
    },
}

/// Split overflow `members` into direct interface items and chunks of the
/// rest (whose members the caller installs on one template per chunk).
fn overflow_plan(members: &[Member]) -> (Vec<OverflowItem>, Vec<Vec<&Member>>) {
    let mut items = Vec::with_capacity(members.len());
    let mut chunks: Vec<Vec<&Member>> = Vec::new();
    for m in members {
        if let MemberKind::Interface(id) = m.kind {
            items.push(OverflowItem::Interface {
                key: m.key.clone(),
                id,
                attrs: m.attrs,
            });
            continue;
        }
        if chunks.last().is_none_or(|c| c.len() >= OVERFLOW_CHUNK) {
            chunks.push(Vec::new());
        }
        let chunk = chunks.len() - 1;
        chunks[chunk].push(m);
        items.push(OverflowItem::Chunk {
            key: m.key.clone(),
            chunk,
        });
    }
    (items, chunks)
}

/// Interfaces with more instance members than this (the window global,
/// ~1,200) get none on the instance template: all go through `overflow`.
///
/// V8 instantiates a template into a fresh fast-mode object and adds the
/// members one by one, each copying the map's descriptor array, so one big
/// template costs O(n²): for the window global about 280M instructions and
/// 60 MB of garbage per realm built without the snapshot. V8 152 also
/// crashes (`PushStackTraceAndDie` in the load IC) past ~1020 properties
/// with accessors (the descriptor limit of a fast-mode map). The global
/// object itself is a dictionary, so defining the members on it after
/// creation is linear; small chunks keep each temporary object cheap (V8
/// keeps the first instance of each template per context, so interface
/// objects, most of the global's members, skip the templates).
const MAX_TEMPLATE_PROPS: usize = 64;
/// Members per overflow chunk template.
const OVERFLOW_CHUNK: usize = 32;

/// How many of the instance members go on the instance template itself.
/// Interface objects never do: they are lazy properties defined after
/// creation, so a list with one goes through `overflow` entirely.
fn template_split(members: &[Member]) -> usize {
    let n = members.len();
    if n > MAX_TEMPLATE_PROPS
        || members
            .iter()
            .any(|m| matches!(m.kind, MemberKind::Interface(_)))
    {
        0
    } else {
        n
    }
}

struct ModuleEntry {
    url: String,
    hash: NonZeroI32,
    module: v8::Global<v8::Module>,
}

/// Which interface objects a realm has made, and what it changes on them.
struct RealmMat {
    made: RefCell<Vec<bool>>,
    fixups: Option<InterfaceFixups>,
}

impl RealmMat {
    fn is_made(&self, id: InterfaceId) -> bool {
        self.made
            .borrow()
            .get(id.0 as usize)
            .copied()
            .unwrap_or(false)
    }
    fn set_made(&self, id: InterfaceId) {
        let mut m = self.made.borrow_mut();
        let i = id.0 as usize;
        if m.len() <= i {
            m.resize(i + 1, false);
        }
        m[i] = true;
    }
    fn fixup(&self, id: InterfaceId) -> Option<InterfaceFixup> {
        self.fixups.as_ref()?.get(&id).cloned()
    }
}

struct RealmEntry {
    context: v8::Global<v8::Context>,
    mat: Rc<RealmMat>,
    /// Deserialized from the startup snapshot context.
    from_snapshot: bool,
    world: World,
    loader: Option<ModuleLoader>,
    modules: Vec<ModuleEntry>,
}

/// Shared with the near-heap-limit callback through a raw pointer.
struct HeapGuard {
    handle: v8::IsolateHandle,
    max: usize,
    hits: Cell<u32>,
    /// Set by the callback, cleared when the termination is reported.
    tripped: Cell<bool>,
    /// Set by the callback, cleared when the limit is restored.
    raised: Cell<bool>,
}

struct State {
    callbacks: Vec<Callback>,
    interfaces: Vec<IfaceSlot>,
    realms: HashMap<RealmId, RealmEntry>,
    /// Realms by the identity hash of their global proxy, so callbacks
    /// find the realm of a context. Kept here rather than in a context
    /// slot: rusty_v8's per-context slot storage (a weak handle per
    /// context) double-frees at isolate teardown for contexts deserialized
    /// from a snapshot.
    realm_by_global: HashMap<i32, Vec<RealmId>>,
    next_realm: u32,
    handles: Slab<(RealmId, v8::Global<v8::Value>)>,
    resolvers: Slab<(RealmId, v8::Global<v8::PromiseResolver>)>,
    wrappers: HashMap<(RealmId, HostRef), v8::Global<v8::Object>>,
    heap: Option<Rc<HeapGuard>>,
    snap: SnapState,
    /// The snapshot context while it is being made (it is no realm).
    creating: Option<Rc<RealmMat>>,
    /// Dropped realms whose context is still alive (see
    /// `drop_realm_state`).
    detached: Vec<(v8::Weak<v8::Context>, Rc<RealmMat>)>,
    /// Large classic scripts compiled in this engine, most recent last,
    /// with their V8 code cache once they were compiled twice (see
    /// `run_script`).
    code_cache: Vec<(u64, Option<Rc<[u8]>>)>,
}

type SharedState = Rc<RefCell<State>>;

/// Classic scripts this long or longer that are compiled a second time
/// get a V8 code cache, used when the same source is compiled again. Playwright
/// evaluates its ~100 KB utility script in the main world of every new
/// document; V8's own compilation cache does not outlive the GC that runs
/// after a page closes, so without this it is parsed and compiled anew
/// each time. Consuming a code cache changes nothing a script can observe.
const CODE_CACHE_MIN: usize = 16 * 1024;
/// Scripts remembered per engine.
const CODE_CACHE_ENTRIES: usize = 8;

impl State {
    fn push_callback(&mut self, cb: Callback) -> u32 {
        self.callbacks.push(cb);
        (self.callbacks.len() - 1) as u32
    }

    fn take_oom(&self) -> bool {
        self.heap.as_ref().is_some_and(|h| h.tripped.replace(false))
    }

    /// A built interface.
    fn entry(&self, id: InterfaceId) -> Option<&IfaceEntry> {
        self.interfaces.get(id.0 as usize)?.built()
    }

    /// The materialization state of a realm.
    fn mat(&self, realm: RealmId) -> Option<Rc<RealmMat>> {
        self.realms.get(&realm).map(|r| r.mat.clone())
    }
}

/// Internal fields of every wrapper: kind, id.
const HOST_FIELDS: usize = 2;

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

/// How [`Engine::build`] creates the isolate.
enum Build {
    Plain,
    Creator,
    Restore(&'static [u8]),
}

/// One V8 isolate with its realms. `!Send`: stays on its thread.
pub struct Engine {
    /// Disposed by hand in `Drop` (a snapshot creator's isolate is consumed
    /// by `create_blob` instead).
    isolate: std::mem::ManuallyDrop<v8::OwnedIsolate>,
    state: SharedState,
    persona: Arc<Persona>,
    /// A snapshot creator holds [`ISOLATE_SETUP`] for its whole life.
    setup: Option<std::sync::MutexGuard<'static, IsolateSetup>>,
    /// Dropped after the isolate (field order).
    live: LiveIsolate,
}

/// Enters the isolate for one operation, so several engines can share a
/// thread. Exits on drop.
struct Entered<'a>(&'a mut v8::OwnedIsolate);

impl<'a> Entered<'a> {
    fn new(isolate: &'a mut v8::OwnedIsolate) -> Self {
        // SAFETY: V8 requires the isolate to be entered on this thread while
        // it is used. Enter/exit nest, and `Drop` pairs this enter with one
        // exit on the same thread (the isolate is `!Send`).
        unsafe { isolate.enter() };
        Entered(isolate)
    }
}

impl Drop for Entered<'_> {
    fn drop(&mut self) {
        // SAFETY: pairs the enter in `Entered::new` on the same thread.
        unsafe { self.0.exit() };
    }
}

impl std::ops::Deref for Entered<'_> {
    type Target = v8::OwnedIsolate;
    fn deref(&self) -> &v8::OwnedIsolate {
        self.0
    }
}

impl std::ops::DerefMut for Entered<'_> {
    fn deref_mut(&mut self) -> &mut v8::OwnedIsolate {
        self.0
    }
}

extern "C" fn near_heap_limit(data: *mut c_void, current: usize, _initial: usize) -> usize {
    // SAFETY: `data` is `Rc::as_ptr` of the engine's `HeapGuard`, which is
    // kept alive in `State::heap` until the callback is removed in
    // `Engine::drop`. V8 calls this on the isolate's own thread.
    let guard = unsafe { &*(data as *const HeapGuard) };
    guard.hits.set(guard.hits.get() + 1);
    guard.tripped.set(true);
    guard.raised.set(true);
    guard.handle.terminate_execution();
    // Headroom so the termination can unwind instead of crashing.
    current + current / 2
}

impl Engine {
    /// Create an engine (isolate) for `persona`. Refuses personas of another
    /// browser family.
    pub fn new(persona: Arc<Persona>, options: EngineOptions) -> Result<Engine, EngineError> {
        Engine::build(persona, options, Build::Plain)
    }

    fn build(
        persona: Arc<Persona>,
        options: EngineOptions,
        build: Build,
    ) -> Result<Engine, EngineError> {
        if persona.family != BrowserFamily::Chrome {
            return Err(EngineError::FamilyMismatch {
                persona: persona.family,
                compiled: Some(BrowserFamily::Chrome),
            });
        }
        init_v8("");
        let mut params = v8::CreateParams::default();
        if let Some(max) = options.max_heap_bytes {
            params = params.heap_limits(0, max);
        }
        let mut snap = SnapState::Off;
        let mut restore_meta = None;
        let mut setup = isolate_setup_lock();
        let build = match build {
            Build::Creator if setup.live > 0 => {
                return Err(EngineError::SnapshotNotAlone);
            }
            // The process already uses another snapshot (or V8's own, with
            // engines alive): this engine must too.
            Build::Restore(blob)
                if setup.blob.is_some_and(|b| !std::ptr::eq(b, blob))
                    || (setup.blob.is_none() && setup.live > 0) =>
            {
                snap = SnapState::Rejected("the process uses another snapshot".into());
                Build::Plain
            }
            b => b,
        };
        // Built engines come from the process's blob too, without replay.
        let plain_blob = match build {
            Build::Plain => setup.blob,
            _ => None,
        };
        let from_blob = |params: v8::CreateParams, blob: &'static [u8]| {
            let (_, v8_part) = Meta::decode(blob).unwrap_or((Meta::default(), blob));
            params
                .external_references(snapshot::external_refs())
                .snapshot_blob(v8::StartupData::from(v8_part))
        };
        let mut isolate = match build {
            Build::Plain => match plain_blob {
                Some(blob) => v8::Isolate::new(from_blob(params, blob)),
                None => v8::Isolate::new(params),
            },
            Build::Creator => {
                snap = SnapState::Creating(Default::default());
                v8::Isolate::snapshot_creator(Some(snapshot::external_refs()), Some(params))
            }
            Build::Restore(blob) => match snapshot::check_blob(blob) {
                Ok((meta, _)) => {
                    restore_meta = Some(meta);
                    setup.blob = Some(blob);
                    v8::Isolate::new(from_blob(params, blob))
                }
                Err(reason) => {
                    snap = SnapState::Rejected(reason);
                    match setup.blob {
                        Some(blob) => v8::Isolate::new(from_blob(params, blob)),
                        None => v8::Isolate::new(params),
                    }
                }
            },
        };
        // `Isolate::new` enters the isolate; undo that so engines can be
        // created and dropped in any order. Every operation enters again.
        // SAFETY: pairs the enter done by `Isolate::new` on this thread.
        unsafe { isolate.exit() };
        let live = LiveIsolate::new(&mut setup);
        let setup = matches!(snap, SnapState::Creating(_)).then_some(setup);
        if let Some(meta) = restore_meta {
            let mut iso = Entered::new(&mut isolate);
            snap = match snapshot::load_templates(&mut iso, meta) {
                Ok(r) => SnapState::Restoring(r),
                Err(reason) => SnapState::Rejected(reason),
            };
        }

        let heap = options.max_heap_bytes.map(|max| {
            Rc::new(HeapGuard {
                handle: isolate.thread_safe_handle(),
                max,
                hits: Cell::new(0),
                tripped: Cell::new(false),
                raised: Cell::new(false),
            })
        });
        let state = Rc::new(RefCell::new(State {
            callbacks: Vec::new(),
            interfaces: Vec::new(),
            realms: HashMap::new(),
            realm_by_global: HashMap::new(),
            next_realm: 1,
            handles: Slab::new(),
            resolvers: Slab::new(),
            wrappers: HashMap::new(),
            heap: heap.clone(),
            snap,
            creating: None,
            detached: Vec::new(),
            code_cache: Vec::new(),
        }));
        {
            let mut iso = Entered::new(&mut isolate);
            iso.set_microtasks_policy(v8::MicrotasksPolicy::Explicit);
            iso.set_slot(state.clone());
            if let Some(heap) = &heap {
                iso.add_near_heap_limit_callback(near_heap_limit, Rc::as_ptr(heap) as *mut c_void);
            }
        }
        Ok(Engine {
            isolate: std::mem::ManuallyDrop::new(isolate),
            state,
            persona,
            setup,
            live,
        })
    }

    /// The persona this engine was created for.
    pub fn persona(&self) -> &Arc<Persona> {
        &self.persona
    }

    /// Handle to terminate running JS from another thread.
    pub fn terminate_handle(&self) -> TerminateHandle {
        TerminateHandle(self.isolate.thread_safe_handle())
    }

    /// The id the next [`Engine::define_interface`] call will return, so a
    /// spec can reference its own interface object (the global's
    /// `Window` property).
    pub fn next_interface_id(&self) -> InterfaceId {
        InterfaceId(self.state.borrow().interfaces.len() as u32)
    }

    /// Define an interface as V8 templates. Templates are per engine; each
    /// realm instantiates them lazily. Define interfaces before the realms
    /// that use them. Panics if `spec.parent` is not an interface of this
    /// engine.
    pub fn define_interface(&mut self, spec: InterfaceSpec) -> InterfaceId {
        let record = {
            let mut st = self.state.borrow_mut();
            if let Some(id) = snapshot::restore_interface(&mut st, &spec) {
                return id;
            }
            matches!(st.snap, SnapState::Creating(_))
                .then(|| (snapshot::spec_digest(&spec), st.callbacks.len() as u32))
        };
        let id = {
            let mut st = self.state.borrow_mut();
            st.interfaces.push(IfaceSlot::Building);
            InterfaceId((st.interfaces.len() - 1) as u32)
        };
        {
            let mut iso = Entered::new(&mut self.isolate);
            v8::scope!(let scope, &mut **iso);
            build_into(scope, &self.state, id, spec);
        }
        if let Some((digest, cb_before)) = record {
            snapshot::record_interface(&mut self.state.borrow_mut(), digest, cb_before);
        }
        id
    }

    /// Like [`Engine::define_interface`], but the spec is made, and the
    /// templates built, only when first needed: by a realm's global (its
    /// interface object, read lazily), a wrapper, a subclass or
    /// [`Arg::Construct`]. The result is the same as an eager definition.
    /// Snapshot creators and restoring engines build at once (the snapshot
    /// records and replays definitions in order).
    pub fn define_interface_lazy(&mut self, make: LazySpec) -> InterfaceId {
        let eager = matches!(
            self.state.borrow().snap,
            SnapState::Creating(_) | SnapState::Restoring(_)
        );
        if eager {
            return self.define_interface(make());
        }
        let mut st = self.state.borrow_mut();
        st.interfaces.push(IfaceSlot::Pending(make));
        InterfaceId((st.interfaces.len() - 1) as u32)
    }

    /// Create a realm (V8 context).
    pub fn new_realm(&mut self, options: RealmOptions) -> Result<RealmId, JsError> {
        let mut iso = Entered::new(&mut self.isolate);
        v8::scope!(let scope, &mut **iso);
        new_realm_in(scope, &self.state, options)
    }

    /// The world a realm belongs to.
    pub fn realm_world(&self, realm: RealmId) -> Option<World> {
        self.state
            .borrow()
            .realms
            .get(&realm)
            .map(|r| r.world.clone())
    }

    /// Drop a realm, its wrappers, handles and resolvers.
    pub fn drop_realm(&mut self, realm: RealmId) {
        drop_realm_state(&mut self.isolate, &self.state, realm);
    }

    /// Run `f` inside `realm`'s context, then clean up termination state.
    fn run<R>(
        &mut self,
        realm: RealmId,
        f: impl for<'s, 'i> FnOnce(&mut v8::PinScope<'s, 'i>, &SharedState) -> Result<R, JsError>,
    ) -> Result<R, JsError> {
        let mut iso = Entered::new(&mut self.isolate);
        let result = {
            v8::scope!(let scope, &mut **iso);
            let context = {
                let st = self.state.borrow();
                let entry = st
                    .realms
                    .get(&realm)
                    .ok_or_else(|| JsError::host(format!("unknown realm {realm:?}")))?;
                v8::Local::new(scope, &entry.context)
            };
            let scope = &mut v8::ContextScope::new(scope, context);
            f(scope, &self.state)
        };
        after_run(&mut iso, &self.state);
        result
    }

    /// Compile and run a classic script; the completion value is copied out.
    pub fn evaluate(
        &mut self,
        realm: RealmId,
        source: &str,
        origin: ScriptOrigin<'_>,
    ) -> Result<JsValue, JsError> {
        self.run(realm, |scope, state| {
            v8::tc_scope!(let tc, scope);
            match run_script(tc, source, origin) {
                Some(v) => Ok(to_js_value(tc, v)),
                None => Err(caught(tc, state)),
            }
        })
    }

    /// Like [`Engine::evaluate`], but keeps the completion value as a handle.
    pub fn evaluate_handle(
        &mut self,
        realm: RealmId,
        source: &str,
        origin: ScriptOrigin<'_>,
    ) -> Result<Handle, JsError> {
        self.run(realm, |scope, state| {
            v8::tc_scope!(let tc, scope);
            match run_script(tc, source, origin) {
                Some(v) => Ok(store_handle(tc, state, realm, v)),
                None => Err(caught(tc, state)),
            }
        })
    }

    /// Compile, link and evaluate an ES module graph. Imports are loaded
    /// through the realm's [`ModuleLoader`]. Returns the module namespace.
    /// A module with top-level await may still be pending afterwards; it
    /// continues in microtask checkpoints.
    pub fn evaluate_module(
        &mut self,
        realm: RealmId,
        source: &str,
        origin: ScriptOrigin<'_>,
    ) -> Result<Handle, JsError> {
        self.run(realm, |scope, state| {
            v8::tc_scope!(let tc, scope);
            let Some(module) = compile_module(tc, state, realm, source, origin) else {
                return Err(caught(tc, state));
            };
            if module.get_status() == v8::ModuleStatus::Uninstantiated
                && module.instantiate_module(tc, resolve_module).is_none()
            {
                return Err(caught(tc, state));
            }
            let Some(result) = module.evaluate(tc) else {
                return Err(caught(tc, state));
            };
            if let Ok(promise) = v8::Local::<v8::Promise>::try_from(result)
                && promise.state() == v8::PromiseState::Rejected
            {
                promise.mark_as_handled();
                let exc = promise.result(tc);
                return Err(exception_to_error(tc, exc, None));
            }
            if module.get_status() == v8::ModuleStatus::Errored {
                let exc = module.get_exception();
                return Err(exception_to_error(tc, exc, None));
            }
            let ns = module.get_module_namespace();
            Ok(store_handle(tc, state, realm, ns))
        })
    }

    /// Call a function held by the host (event listener, timer callback).
    pub fn call(
        &mut self,
        realm: RealmId,
        func: Handle,
        this: Arg,
        args: &[Arg],
    ) -> Result<JsValue, JsError> {
        self.run(realm, |scope, state| {
            v8::tc_scope!(let tc, scope);
            let v = call_inner(tc, state, realm, func, &this, args)?;
            match v {
                Some(v) => Ok(to_js_value(tc, v)),
                None => Err(caught(tc, state)),
            }
        })
    }

    /// Like [`Engine::call`], but keeps the return value as a handle.
    pub fn call_handle(
        &mut self,
        realm: RealmId,
        func: Handle,
        this: Arg,
        args: &[Arg],
    ) -> Result<Handle, JsError> {
        self.run(realm, |scope, state| {
            v8::tc_scope!(let tc, scope);
            let v = call_inner(tc, state, realm, func, &this, args)?;
            match v {
                Some(v) => Ok(store_handle(tc, state, realm, v)),
                None => Err(caught(tc, state)),
            }
        })
    }

    /// Copy a held value out (objects as JSON).
    pub fn get_value(&mut self, realm: RealmId, handle: Handle) -> Result<JsValue, JsError> {
        self.run(realm, |scope, state| {
            let v = load_handle(scope, state, handle)?;
            Ok(to_js_value(scope, v))
        })
    }

    /// Read a property of a held object (`obj[key]`) as a new handle.
    pub fn get_property(
        &mut self,
        realm: RealmId,
        object: Handle,
        key: &str,
    ) -> Result<Handle, JsError> {
        self.run(realm, |scope, state| {
            v8::tc_scope!(let tc, scope);
            let v = load_handle(tc, state, object)?;
            let obj = v.to_object(tc).ok_or_else(|| caught(tc, state))?;
            let k = v8_str(tc, key);
            match obj.get(tc, k.into()) {
                Some(v) => Ok(store_handle(tc, state, realm, v)),
                None => Err(caught(tc, state)),
            }
        })
    }

    /// Forget a handle. Releasing twice is harmless.
    pub fn release(&mut self, handle: Handle) {
        self.state
            .borrow_mut()
            .handles
            .remove(handle.idx, handle.generation);
    }

    /// The wrapper of a host object in `realm`, created on first use.
    pub fn wrap(
        &mut self,
        realm: RealmId,
        iface: InterfaceId,
        host: HostRef,
    ) -> Result<Handle, JsError> {
        self.run(realm, |scope, state| {
            let obj = wrap_host(scope, state, realm, iface, host)?;
            Ok(store_handle(scope, state, realm, obj.into()))
        })
    }

    /// The host object behind a held JS value, if it is a wrapper.
    pub fn unwrap_host(
        &mut self,
        realm: RealmId,
        handle: Handle,
    ) -> Result<Option<HostRef>, JsError> {
        self.run(realm, |scope, state| {
            let v = load_handle(scope, state, handle)?;
            Ok(v8::Local::<v8::Object>::try_from(v)
                .ok()
                .and_then(|o| read_host(scope, o)))
        })
    }

    /// Drop the cached wrappers of a host object in every realm (the host
    /// object died). A later wrap creates a fresh wrapper.
    pub fn forget_host(&mut self, host: HostRef) {
        self.state
            .borrow_mut()
            .wrappers
            .retain(|(_, h), _| *h != host);
    }

    /// A new pending promise in `realm` and its resolver.
    pub fn new_promise(&mut self, realm: RealmId) -> Result<(Handle, Resolver), JsError> {
        self.run(realm, |scope, state| {
            let (promise, resolver) = new_promise(scope, state, realm)
                .ok_or_else(|| JsError::host("promise creation failed"))?;
            Ok((store_handle(scope, state, realm, promise.into()), resolver))
        })
    }

    /// Resolve a promise created by [`Engine::new_promise`] or
    /// [`CallCtx::ret_new_promise`]. Reactions run at the next checkpoint.
    pub fn resolve_promise(&mut self, resolver: Resolver, value: Arg) -> Result<(), JsError> {
        self.settle(resolver, Ok(value))
    }

    /// Reject a promise with an exception value.
    pub fn reject_promise(&mut self, resolver: Resolver, error: HostError) -> Result<(), JsError> {
        self.settle(resolver, Err(error))
    }

    fn settle(
        &mut self,
        resolver: Resolver,
        outcome: Result<Arg, HostError>,
    ) -> Result<(), JsError> {
        let realm = self
            .state
            .borrow()
            .resolvers
            .get(resolver.idx, resolver.generation)
            .map(|(r, _)| *r)
            .ok_or_else(|| JsError::host("stale resolver"))?;
        self.run(realm, |scope, state| {
            v8::tc_scope!(let tc, scope);
            match settle_promise(tc, state, realm, resolver, outcome) {
                Ok(()) => Ok(()),
                Err(HostError::Thrown) => Err(caught(tc, state)),
                Err(e) => Err(JsError::host(format!("{e:?}"))),
            }
        })
    }

    /// A native function in `realm` backed by `f` (`name` and `length` as
    /// given; `toString` prints `[native code]`). Create these once per
    /// realm (each call registers a callback for the engine's lifetime).
    pub fn new_function(
        &mut self,
        realm: RealmId,
        name: &str,
        length: u32,
        f: HostFn,
    ) -> Result<Handle, JsError> {
        let idx = self.state.borrow_mut().push_callback(Callback::Fn(f));
        self.run(realm, |scope, state| {
            let data = v8::Integer::new_from_unsigned(scope, idx);
            let func = v8::Function::builder(host_trampoline)
                .data(data.into())
                .length(length as i32)
                .constructor_behavior(v8::ConstructorBehavior::Throw)
                .build(scope)
                .ok_or_else(|| JsError::host("function creation failed"))?;
            let n = v8_str(scope, name);
            func.set_name(n);
            Ok(store_handle(scope, state, realm, func.into()))
        })
    }

    /// Like [`Engine::new_function`], but `new f()` works and `f` has a
    /// `prototype` (V8 extension functions such as Chrome's
    /// `chrome.loadTimes`). A construct call runs `f` with a fresh `this`.
    pub fn new_constructible_function(
        &mut self,
        realm: RealmId,
        name: &str,
        length: u32,
        f: HostFn,
    ) -> Result<Handle, JsError> {
        let idx = self.state.borrow_mut().push_callback(Callback::Fn(f));
        self.run(realm, |scope, state| {
            let data = v8::Integer::new_from_unsigned(scope, idx);
            let func = v8::Function::builder(host_trampoline)
                .data(data.into())
                .length(length as i32)
                .constructor_behavior(v8::ConstructorBehavior::Allow)
                .build(scope)
                .ok_or_else(|| JsError::host("function creation failed"))?;
            let n = v8_str(scope, name);
            func.set_name(n);
            Ok(store_handle(scope, state, realm, func.into()))
        })
    }

    /// Make `time_zone` (an IANA id such as `America/New_York`) the zone
    /// of `Date` and `Intl` in this engine. Returns false for an id ICU
    /// does not know.
    ///
    /// ICU's default zone is process-wide. This sets it (when it differs)
    /// and resets this isolate's date cache. `Date` keeps the zone it
    /// cached until the next reset of its own isolate, but `Intl` reads the
    /// process default on every new formatter, so engines on other threads
    /// see the new zone in `Intl` immediately. Call it before running a
    /// page whose persona has a different zone than the last one.
    pub fn set_time_zone(&mut self, time_zone: &str) -> bool {
        if !set_icu_time_zone(time_zone) {
            return false;
        }
        let mut iso = Entered::new(&mut self.isolate);
        iso.date_time_configuration_change_notification(v8::TimeZoneDetection::Skip);
        true
    }

    /// Set ICU's default locale (`Intl` default, e.g. `en-US`). Process
    /// wide; an isolate caches its default locale on first `Intl` use, so
    /// call it before the first page script of the engine.
    pub fn set_default_locale(&mut self, locale: &str) {
        if !locale.is_empty() && !locale.contains('\0') && v8::icu::get_language_tag() != locale {
            // ICU locale ids use `_` (`en_US`).
            v8::icu::set_default_locale(&locale.replace('-', "_"));
        }
    }

    /// Materialize an [`Arg`] as a held value in `realm`.
    pub fn make_handle(&mut self, realm: RealmId, value: Arg) -> Result<Handle, JsError> {
        self.run(realm, |scope, state| {
            v8::tc_scope!(let tc, scope);
            match arg_to_v8(tc, state, realm, &value) {
                Ok(v) => Ok(store_handle(tc, state, realm, v)),
                Err(_) => Err(caught(tc, state)),
            }
        })
    }

    /// The realm's global object as a handle.
    pub fn global(&mut self, realm: RealmId) -> Result<Handle, JsError> {
        self.run(realm, |scope, state| {
            let g = scope.get_current_context().global(scope);
            Ok(store_handle(scope, state, realm, g.into()))
        })
    }

    /// `obj[key] = value` on a held object (no-op for non-objects).
    pub fn set_property(
        &mut self,
        realm: RealmId,
        object: Handle,
        key: &str,
        value: Arg,
    ) -> Result<(), JsError> {
        self.run(realm, |scope, state| {
            v8::tc_scope!(let tc, scope);
            let v = load_handle(tc, state, object)?;
            if let Ok(o) = v8::Local::<v8::Object>::try_from(v) {
                let val = match arg_to_v8(tc, state, realm, &value) {
                    Ok(v) => v,
                    Err(_) => return Err(caught(tc, state)),
                };
                let k = v8_str(tc, key);
                if o.set(tc, k.into(), val).is_none() {
                    return Err(caught(tc, state));
                }
            }
            Ok(())
        })
    }

    /// Queue a held function as a microtask of its realm.
    pub fn enqueue_microtask(&mut self, realm: RealmId, func: Handle) -> Result<(), JsError> {
        self.run(realm, |scope, state| {
            let v = load_handle(scope, state, func)?;
            let f = v8::Local::<v8::Function>::try_from(v)
                .map_err(|_| JsError::host("handle is not a function"))?;
            scope.enqueue_microtask(f);
            Ok(())
        })
    }

    /// Type of a held value.
    pub fn value_kind(&mut self, realm: RealmId, handle: Handle) -> Result<ValueKind, JsError> {
        self.run(realm, |scope, state| {
            Ok(value_kind(load_handle(scope, state, handle)?))
        })
    }

    /// Run all queued promise jobs (explicit microtask policy).
    pub fn perform_microtask_checkpoint(&mut self) {
        let mut iso = Entered::new(&mut self.isolate);
        iso.perform_microtask_checkpoint();
        after_run(&mut iso, &self.state);
    }

    /// Tell V8 the page is idle: collect garbage aggressively.
    pub fn low_memory_notification(&mut self) {
        let mut iso = Entered::new(&mut self.isolate);
        iso.low_memory_notification();
    }

    /// One full, memory-reducing GC (V8's critical memory-pressure path:
    /// compaction, new space shrunk, unused pages released), then back to
    /// no pressure. Cheaper than [`Engine::low_memory_notification`], which
    /// repeats full GCs until nothing more is freed and also drops the
    /// compilation cache.
    pub fn reduce_memory(&mut self) {
        let mut iso = Entered::new(&mut self.isolate);
        iso.memory_pressure_notification(v8::MemoryPressureLevel::Critical);
        iso.memory_pressure_notification(v8::MemoryPressureLevel::None);
    }

    pub fn heap_stats(&mut self) -> HeapStats {
        let limit_hits = self
            .state
            .borrow()
            .heap
            .as_ref()
            .map_or(0, |h| h.hits.get());
        let mut iso = Entered::new(&mut self.isolate);
        let s = iso.get_heap_statistics();
        HeapStats {
            used: s.used_heap_size(),
            total: s.total_heap_size(),
            limit: s.heap_size_limit(),
            limit_hits,
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let heap = {
            let mut st = self.state.borrow_mut();
            st.wrappers.clear();
            st.handles.clear();
            st.resolvers.clear();
            st.realms.clear();
            st.detached.clear();
            st.interfaces.clear();
            st.callbacks.clear();
            st.heap.take()
        };
        // SAFETY: `OwnedIsolate::drop` asserts the isolate is the current one
        // and exits it once; this enter balances that exit.
        unsafe { self.isolate.enter() };
        if let Some(heap) = heap {
            self.isolate
                .remove_near_heap_limit_callback(near_heap_limit, heap.max);
        }
        self.isolate.remove_slot::<SharedState>();
        // SAFETY: taken once, here; the field is never used again.
        let isolate = unsafe { std::mem::ManuallyDrop::take(&mut self.isolate) };
        if self.setup.is_some() {
            // A snapshot creator that never made its blob: V8 only lets it
            // go through `CreateBlob`, which is not safe here. Leak it.
            // SAFETY: balances the enter above; the isolate is never used.
            unsafe { isolate.exit() };
            std::mem::forget(isolate);
            // It still exists: keep counting it.
            if let Some(setup) = self.setup.as_mut() {
                let extra = LiveIsolate::new(setup);
                std::mem::forget(extra);
            }
        } else {
            drop(isolate);
        }
    }
}

/// Build the V8 templates of interface `self_id` (its slot exists) from
/// `spec`; the templates it refers to are built first.
fn build_into(
    scope: &mut v8::PinScope<'_, '_, ()>,
    state: &SharedState,
    self_id: InterfaceId,
    spec: InterfaceSpec,
) {
    let deps: Vec<InterfaceId> = spec
        .parent
        .into_iter()
        .chain(
            // Instance lists never hold interface objects as template
            // values (`template_split`): those stay lazy.
            spec.statics
                .iter()
                .chain(&spec.prototype)
                .filter_map(|m| match m.kind {
                    MemberKind::Interface(id) if id != self_id => Some(id),
                    _ => None,
                }),
        )
        .collect();
    for d in deps {
        ensure_template(scope, state, d);
    }
    let mut st = state.borrow_mut();
    let name: Rc<str> = spec.name.as_str().into();
    let ctor_idx = st.push_callback(Callback::Ctor {
        iface: name.clone(),
        f: spec.constructor.clone(),
    });
    let data = v8::Integer::new_from_unsigned(scope, ctor_idx);
    let ft = v8::FunctionTemplate::builder(ctor_trampoline)
        .data(data.into())
        .length(spec.length as i32)
        .build(scope);
    ft.set_class_name(v8_str(scope, &name));
    // A legacy factory function's `prototype` is replaced per realm
    // (`interface_object`), which a read-only one would refuse.
    if spec.prototype_from.is_none() {
        ft.read_only_prototype();
    }
    if let Some(parent) = spec.parent {
        let parent_ft = v8::Local::new(
            scope,
            &st.entry(parent)
                .expect("parent interface defined first")
                .template,
        );
        ft.inherit(parent_ft);
    }
    let signature = v8::Signature::new(scope, ft);
    let inst = ft.instance_template(scope);
    inst.set_internal_field_count(HOST_FIELDS);
    let proto = ft.prototype_template(scope);

    let mut methods = Vec::new();
    let this_iface = (self_id, ft);
    for m in &spec.statics {
        install_member(
            scope,
            &mut st,
            Target::Function(ft),
            None,
            m,
            &mut methods,
            this_iface,
        );
    }
    methods.clear();
    for m in &spec.prototype {
        install_member(
            scope,
            &mut st,
            Target::Object(proto),
            Some(signature),
            m,
            &mut methods,
            this_iface,
        );
    }
    if spec.to_string_tag {
        let tag = v8::Symbol::get_to_string_tag(scope);
        let value = v8_str(scope, &name);
        proto.set_with_attr(
            tag.into(),
            value.into(),
            v8::PropertyAttribute::READ_ONLY | v8::PropertyAttribute::DONT_ENUM,
        );
    }
    methods.clear();
    let split = template_split(&spec.instance);
    for m in &spec.instance[..split] {
        install_member(
            scope,
            &mut st,
            Target::Object(inst),
            Some(signature),
            m,
            &mut methods,
            this_iface,
        );
    }
    let (items, plan) = overflow_plan(&spec.instance[split..]);
    let mut made: Vec<Option<Direct>> = Vec::new();
    let chunks = plan
        .into_iter()
        .map(|chunk| {
            let ot = v8::ObjectTemplate::new(scope);
            for m in chunk {
                let r = install_member(
                    scope,
                    &mut st,
                    Target::Object(ot),
                    Some(signature),
                    m,
                    &mut methods,
                    this_iface,
                );
                let g = |ft: v8::Local<'_, v8::FunctionTemplate>| v8::Global::new(scope, ft);
                made.push(match (r, &m.kind) {
                    (Some(Made::Method(ft)), _) => Some(Direct::Method(g(ft), m.attrs)),
                    (Some(Made::Accessor(get, set)), _) => {
                        Some(Direct::Accessor(get.map(g), set.map(g), m.attrs))
                    }
                    (Some(Made::Value), MemberKind::Value(v)) => {
                        Some(Direct::Value(v.clone(), m.attrs))
                    }
                    _ => None,
                });
            }
            v8::Global::new(scope, ot)
        })
        .collect();
    let mut made = made.into_iter();
    let direct = items
        .iter()
        .map(|it| match it {
            OverflowItem::Chunk { .. } => made.next().flatten(),
            OverflowItem::Interface { .. } => None,
        })
        .collect();
    let overflow = Overflow {
        chunks,
        items,
        direct,
    };
    if spec.immutable_proto {
        inst.set_immutable_proto();
    }
    if let Some(getter) = spec.named_getter {
        // The named callbacks sit at consecutive indexes (get, set,
        // delete, enum) so one integer of data reaches all four.
        let has = (
            spec.named_setter.is_some(),
            spec.named_deleter.is_some(),
            spec.named_enumerator.is_some(),
        );
        let base = st.push_callback(Callback::Named(getter));
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
        let data = v8::Integer::new_from_unsigned(scope, base);
        let mut config = v8::NamedPropertyHandlerConfiguration::new()
            .getter(named_trampoline)
            .data(data.into())
            .flags(v8::PropertyHandlerFlags::ONLY_INTERCEPT_STRINGS);
        if has.0 {
            config = config.setter(named_set_trampoline);
        }
        if has.1 {
            config = config.deleter(named_delete_trampoline);
        }
        if has.2 {
            config = config.enumerator(named_enum_trampoline);
        }
        if spec.named_unenumerable {
            config = config.query(named_query_trampoline);
        }
        inst.set_named_property_handler(config);
    }
    if let Some(getter) = spec.indexed_getter {
        // The length callback, if any, sits right after the getter.
        let idx = st.push_callback(Callback::Indexed(getter));
        let mut config = v8::IndexedPropertyHandlerConfiguration::new().getter(indexed_trampoline);
        if let Some(len) = spec.indexed_length {
            st.push_callback(Callback::IndexedLen(len));
            config = config
                .query(indexed_query_trampoline)
                .enumerator(indexed_enum_trampoline)
                .setter(indexed_set_trampoline)
                .deleter(indexed_delete_trampoline);
        }
        let data = v8::Integer::new_from_unsigned(scope, idx);
        inst.set_indexed_property_handler(config.data(data.into()));
    }

    let template = v8::Global::new(scope, ft);
    st.interfaces[self_id.0 as usize] = IfaceSlot::Built(IfaceEntry {
        template,
        parent: spec.parent,
        prototype_from: spec.prototype_from,
        overflow,
        reorder_last: spec.reorder_last,
    });
}

/// The templates of `id`, built now if they were defined lazily. `None`
/// for an unknown interface (or one that is its own ancestor).
fn ensure_template<'s>(
    scope: &mut v8::PinScope<'s, '_, ()>,
    state: &SharedState,
    id: InterfaceId,
) -> Option<v8::Local<'s, v8::FunctionTemplate>> {
    let make = {
        let mut st = state.borrow_mut();
        let slot = st.interfaces.get_mut(id.0 as usize)?;
        match slot {
            IfaceSlot::Built(e) => return Some(v8::Local::new(scope, &e.template)),
            IfaceSlot::Building => return None,
            IfaceSlot::Pending(_) => match std::mem::replace(slot, IfaceSlot::Building) {
                IfaceSlot::Pending(make) => make,
                _ => unreachable!(),
            },
        }
    };
    let spec = make();
    build_into(scope, state, id, spec);
    let st = state.borrow();
    st.entry(id).map(|e| v8::Local::new(scope, &e.template))
}

/// Make `time_zone` ICU's (process-wide) default zone unless it already
/// is. The default changes only here, so the last zone set says whether
/// it is: asking ICU instead would make it detect the host's zone first
/// (its data paged in, about 0.5 ms per process).
fn set_icu_time_zone(time_zone: &str) -> bool {
    static LAST: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if last.as_deref() == Some(time_zone) {
        return true;
    }
    if !v8::icu::set_default_time_zone(time_zone) {
        return false;
    }
    *last = Some(time_zone.to_owned());
    true
}

/// Body of [`Engine::new_realm`], shared with [`RealmOps`] for `CallCtx`.
fn new_realm_in(
    scope: &mut v8::PinScope<'_, '_, ()>,
    state: &SharedState,
    options: RealmOptions,
) -> Result<RealmId, JsError> {
    if let Some(id) = options.global
        && state.borrow().interfaces.get(id.0 as usize).is_none()
    {
        return Err(JsError::host(format!("unknown interface {id:?}")));
    }
    let snapshot_global = state.borrow().snap.context_global();
    let from_snapshot = options.global.is_some() && options.global == snapshot_global;
    let context = if from_snapshot {
        v8::Context::from_snapshot(scope, 0, Default::default())
            .ok_or_else(|| JsError::host("snapshot context could not be created"))?
    } else {
        new_context(scope, state, options.global)
    };
    let made = if from_snapshot {
        state.borrow().snap.context_made()
    } else {
        Vec::new()
    };
    let mat = Rc::new(RealmMat {
        made: RefCell::new(made),
        fixups: options.fixups.clone(),
    });
    let id = {
        let mut st = state.borrow_mut();
        let id = RealmId(st.next_realm);
        st.next_realm += 1;
        id
    };
    let scope = &mut v8::ContextScope::new(scope, context);
    let global = context.global(scope);
    {
        let mut st = state.borrow_mut();
        st.realm_by_global
            .entry(global.get_identity_hash().get())
            .or_default()
            .push(id);
        if let Some(host) = options.global_host {
            if global.internal_field_count() >= HOST_FIELDS {
                set_host(scope, global, host);
            }
            st.wrappers
                .insert((id, host), v8::Global::new(scope, global));
        }
        st.realms.insert(
            id,
            RealmEntry {
                context: v8::Global::new(scope, context),
                mat: mat.clone(),
                from_snapshot,
                world: options.world,
                loader: options.module_loader,
                modules: Vec::new(),
            },
        );
    }
    let result = match options.global {
        Some(g) if from_snapshot => restore_global(scope, state, &mat, g, global),
        Some(g) => setup_global(scope, state, &mat, g, global),
        None => Ok(()),
    };
    if let Err(e) = result {
        drop_realm_state(scope, state, id);
        return Err(e);
    }
    Ok(id)
}

/// A new context whose global is made from `global`'s instance template
/// (members and interface objects are added by [`setup_global`]).
fn new_context<'s>(
    scope: &mut v8::PinScope<'s, '_, ()>,
    state: &SharedState,
    global: Option<InterfaceId>,
) -> v8::Local<'s, v8::Context> {
    let global_template = global
        .and_then(|id| ensure_template(scope, state, id))
        .map(|ft| ft.instance_template(scope));
    v8::Context::new(
        scope,
        v8::ContextOptions {
            global_template,
            ..Default::default()
        },
    )
}

/// Finish the global of a new context (entered): link and fix the
/// interface objects V8 made with it (the global interface and its
/// ancestors), define the global's members (interface objects as lazy
/// properties) and move `reorder_last` to the end. Also makes the
/// snapshot context.
fn setup_global<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    state: &SharedState,
    mat: &RealmMat,
    iface: InterfaceId,
    global: v8::Local<'s, v8::Object>,
) -> Result<(), JsError> {
    interface_object(scope, state, mat, iface);
    let last = state
        .borrow()
        .entry(iface)
        .map(|e| e.reorder_last.clone())
        .unwrap_or_default();
    apply_overflow_with(scope, state, iface, global, &last)
}

/// The global of a context deserialized from the snapshot (entered): the
/// interface objects made in the snapshot context had their base fixups
/// there, this realm's removals run now; then `reorder_last`, because V8
/// appends its flag-gated built-ins to every new context.
fn restore_global<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    state: &SharedState,
    mat: &RealmMat,
    iface: InterfaceId,
    global: v8::Local<'s, v8::Object>,
) -> Result<(), JsError> {
    if let Some(fixups) = &mat.fixups {
        let made: Vec<InterfaceId> = {
            let m = mat.made.borrow();
            (0..m.len())
                .filter(|&i| m[i])
                .map(|i| InterfaceId(i as u32))
                .collect()
        };
        for id in made {
            let Some(fx) = fixups.get(&id) else { continue };
            if fx.remove_proto.is_empty() && fx.remove_static.is_empty() {
                continue;
            }
            let Some(f) = ensure_template(scope, state, id).and_then(|ft| ft.get_function(scope))
            else {
                continue;
            };
            apply_fixup(scope, f, fx, false, true);
        }
    }
    reorder_last(scope, state, iface, global)
}

/// The interface object of `id` in the current context (realm state
/// `mat`), made on first use: ancestors first, then the WebIDL link
/// `[[Prototype]]` = parent interface object (V8 templates only link the
/// prototype objects), then the realm's fixups. Every path that makes an
/// interface object goes through here (the global's lazy properties,
/// wrappers, [`Arg::Construct`]), so V8's per-context template cache
/// returns the fixed object everywhere else.
fn interface_object<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    state: &SharedState,
    mat: &RealmMat,
    id: InterfaceId,
) -> Option<v8::Local<'s, v8::Function>> {
    let ft = ensure_template(scope, state, id)?;
    if mat.is_made(id) {
        return ft.get_function(scope);
    }
    let parent = state.borrow().entry(id).and_then(|e| e.parent);
    let pf = match parent {
        Some(p) => Some(interface_object(scope, state, mat, p)?),
        None => None,
    };
    let f = ft.get_function(scope)?;
    if let Some(pf) = pf {
        f.set_prototype(scope, pf.into());
    }
    mat.set_made(id);
    let from = state.borrow().entry(id).and_then(|e| e.prototype_from);
    if let Some(target) = from
        && let Some(tf) = interface_object(scope, state, mat, target)
    {
        let key = v8_str(scope, "prototype");
        if let Some(proto) = tf.get(scope, key.into()) {
            // Like Blink's legacy factory functions: the interface's own
            // prototype object, read-only, not enumerable or configurable.
            // Defining it through the function's `prototype` accessor also
            // sets the initial map, so `new.target` subclasses still work.
            let mut desc = v8::PropertyDescriptor::new_from_value_writable(proto, false);
            desc.set_enumerable(false);
            desc.set_configurable(false);
            f.define_property(scope, key.into(), &desc);
        }
    }
    if let Some(fx) = mat.fixup(id) {
        apply_fixup(scope, f, &fx, true, true);
    }
    Some(f)
}

/// Run a fixup on interface object `f`: the per-global `base` part
/// (late members, unscopables, error prototype) and/or this realm's
/// `removals`, in the order the old init script ran them.
fn apply_fixup<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    f: v8::Local<'s, v8::Function>,
    fx: &InterfaceFixup,
    base: bool,
    removals: bool,
) {
    let key = v8_str(scope, "prototype");
    let proto = f
        .get(scope, key.into())
        .and_then(|p| v8::Local::<v8::Object>::try_from(p).ok());
    if let Some(proto) = proto {
        if base {
            for k in &fx.proto_late {
                let key = v8_str(scope, k);
                let _ = move_to_end(scope, proto, key.into());
            }
            if !fx.unscopables.is_empty() {
                let u = v8::Object::new(scope);
                let null = v8::null(scope);
                u.set_prototype(scope, null.into());
                for k in &fx.unscopables {
                    let key = v8_str(scope, k);
                    let t = v8::Boolean::new(scope, true);
                    u.set(scope, key.into(), t.into());
                }
                let sym = v8::Symbol::get_unscopables(scope);
                let mut desc = v8::PropertyDescriptor::new_from_value_writable(u.into(), false);
                desc.set_enumerable(false);
                desc.set_configurable(true);
                proto.define_property(scope, sym.into(), &desc);
            }
            if fx.error_proto {
                // The realm's own Error.prototype, whatever `globalThis.Error`
                // is by now.
                let msg = v8::String::empty(scope);
                let err = v8::Exception::error(scope, msg);
                if let Some(ep) = err.to_object(scope).and_then(|o| o.get_prototype(scope)) {
                    proto.set_prototype(scope, ep);
                }
            }
        }
        if removals {
            for k in &fx.remove_proto {
                let key = v8_str(scope, k);
                proto.delete(scope, key.into());
            }
        }
    }
    if removals {
        for k in &fx.remove_static {
            let key = v8_str(scope, k);
            f.delete(scope, key.into());
        }
    }
}

/// A property descriptor object (from `getOwnPropertyDescriptor`) as a
/// descriptor to define with.
fn copy_descriptor<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    d: v8::Local<'s, v8::Object>,
) -> v8::PropertyDescriptor {
    let field = |scope: &mut v8::PinScope<'s, '_>, name: &str| -> v8::Local<'s, v8::Value> {
        v8_str(scope, name).into()
    };
    let (k_enum, k_conf, k_get, k_set, k_value, k_writable) = (
        field(scope, "enumerable"),
        field(scope, "configurable"),
        field(scope, "get"),
        field(scope, "set"),
        field(scope, "value"),
        field(scope, "writable"),
    );
    descriptor_with(
        scope,
        d,
        [k_enum, k_conf, k_get, k_set, k_value, k_writable],
    )
}

/// [`copy_descriptor`] with the field names made once by the caller:
/// `enumerable`, `configurable`, `get`, `set`, `value`, `writable`.
fn descriptor_with<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    d: v8::Local<'s, v8::Object>,
    [k_enum, k_conf, k_get, k_set, k_value, k_writable]: [v8::Local<'s, v8::Value>; 6],
) -> v8::PropertyDescriptor {
    let enumerable = d.get(scope, k_enum).is_some_and(|v| v.is_true());
    let configurable = d.get(scope, k_conf).is_some_and(|v| v.is_true());
    let get = d.get(scope, k_get);
    let set = d.get(scope, k_set);
    let mut desc = match (get, set) {
        (Some(g), Some(s)) if g.is_function() || s.is_function() => {
            v8::PropertyDescriptor::new_from_get_set(g, s)
        }
        _ => {
            let value = d
                .get(scope, k_value)
                .unwrap_or_else(|| v8::undefined(scope).into());
            let writable = d.get(scope, k_writable).is_some_and(|v| v.is_true());
            v8::PropertyDescriptor::new_from_value_writable(value, writable)
        }
    };
    desc.set_enumerable(enumerable);
    desc.set_configurable(configurable);
    desc
}

/// Move an own property to the end of `obj`'s property order, keeping its
/// descriptor (delete, then define again). `Ok(false)` if there is none;
/// an error if it cannot be deleted.
fn move_to_end<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    obj: v8::Local<'s, v8::Object>,
    key: v8::Local<'s, v8::Name>,
) -> Result<bool, JsError> {
    let Some(d) = obj.get_own_property_descriptor(scope, key) else {
        return Ok(false);
    };
    let Ok(d) = v8::Local::<v8::Object>::try_from(d) else {
        return Ok(false);
    };
    let desc = copy_descriptor(scope, d);
    if obj.delete(scope, key.into()) != Some(true) {
        return Err(JsError::host(format!(
            "cannot move {} to the end",
            key.to_rust_string_lossy(scope)
        )));
    }
    obj.define_property(scope, key, &desc);
    Ok(true)
}

/// Define interface object `id` as a lazy data property of `obj` (as
/// Blink does for the global's interface objects): it looks and behaves
/// like a plain data property; the object is made on first read.
fn define_lazy_interface<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    obj: v8::Local<'s, v8::Object>,
    key: v8::Local<'s, v8::Name>,
    id: InterfaceId,
    attrs: PropAttrs,
) {
    let data = v8::Integer::new_from_unsigned(scope, id.0);
    obj.set_lazy_data_property_with_data(
        scope,
        key,
        lazy_interface_getter,
        data.into(),
        v8_attrs(attrs, false),
        v8::SideEffectType::HasNoSideEffect,
        v8::SideEffectType::HasSideEffectToReceiver,
    );
}

/// Move the global interface's `reorder_last` keys to the end of
/// `global`, in order (a global from the snapshot). Interface objects
/// among them stay lazy.
fn reorder_last<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    state: &SharedState,
    iface: InterfaceId,
    global: v8::Local<'s, v8::Object>,
) -> Result<(), JsError> {
    let keys = state
        .borrow()
        .entry(iface)
        .map(|e| e.reorder_last.clone())
        .unwrap_or_default();
    move_globals_last(scope, state, iface, global, &keys)
}

/// Move own properties of a global made from `iface` to the end, in the
/// order of `keys` (absent ones are skipped; one that cannot be deleted
/// is an error, as `delete` in strict code). The global's interface
/// objects are defined again as lazy properties instead of being read.
fn move_globals_last<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    state: &SharedState,
    iface: InterfaceId,
    global: v8::Local<'s, v8::Object>,
    keys: &[PropKey],
) -> Result<(), JsError> {
    let plan: Vec<Option<(InterfaceId, PropAttrs)>> = {
        let st = state.borrow();
        let lazy: HashMap<&PropKey, (InterfaceId, PropAttrs)> = st
            .entry(iface)
            .map(|entry| {
                entry
                    .overflow
                    .items
                    .iter()
                    .filter_map(|i| match i {
                        OverflowItem::Interface { key, id, attrs } => Some((key, (*id, *attrs))),
                        OverflowItem::Chunk { .. } => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        keys.iter().map(|k| lazy.get(k).copied()).collect()
    };
    for (key, lazy) in keys.iter().zip(plan) {
        let k = v8_key(scope, key);
        match lazy {
            Some((id, attrs)) => {
                if global.has_own_property(scope, k) != Some(true) {
                    continue;
                }
                if global.delete(scope, k.into()) != Some(true) {
                    return Err(JsError::host("cannot move an interface object"));
                }
                define_lazy_interface(scope, global, k, id, attrs);
            }
            None => {
                move_to_end(scope, global, k)?;
            }
        }
    }
    Ok(())
}

/// Getter of the global's lazy interface-object properties: makes the
/// interface object in the global's own realm (also when read from
/// another realm, e.g. a frame's `contentWindow.Node`). V8 then replaces
/// the property with a plain data property holding the value.
fn lazy_interface_getter<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    _key: v8::Local<'s, v8::Name>,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<v8::Value>,
) {
    let state = shared_state(scope);
    let id = InterfaceId(callback_index(scope, args.data()) as u32);
    let Some(ctx) = args.holder().get_creation_context(scope) else {
        return;
    };
    let creating = state.borrow().creating.clone();
    let mat = match creating {
        Some(m) => m,
        None => {
            let live = realm_of_context(scope, ctx).and_then(|r| state.borrow().mat(r));
            let mat = live.or_else(|| {
                let st = state.borrow();
                st.detached
                    .iter()
                    .find(|(w, _)| w.to_local(scope) == Some(ctx))
                    .map(|(_, m)| m.clone())
            });
            let Some(m) = mat else {
                return;
            };
            m
        }
    };
    let scope = &mut v8::ContextScope::new(scope, ctx);
    if let Some(f) = interface_object(scope, &state, &mat, id) {
        rv.set(f.into());
    }
}

/// Whether `realm` was deserialized from the snapshot context.
fn is_snapshot_realm(state: &SharedState, realm: RealmId) -> bool {
    state
        .borrow()
        .realms
        .get(&realm)
        .is_some_and(|r| r.from_snapshot)
}

fn drop_realm_state(isolate: &mut v8::Isolate, state: &SharedState, realm: RealmId) {
    let mut st = state.borrow_mut();
    if let Some(entry) = st.realms.remove(&realm) {
        // Scripts may still hold the realm's global (a removed frame's
        // `contentWindow`): its lazy interface objects must still be made
        // with this realm's fixups. Kept until the context is collected.
        st.detached.retain(|(w, _)| !w.is_empty());
        let weak = v8::Weak::new(isolate, &entry.context);
        st.detached.push((weak, entry.mat));
    }
    st.realm_by_global.retain(|_, ids| {
        ids.retain(|r| *r != realm);
        !ids.is_empty()
    });
    st.wrappers.retain(|(r, _), _| *r != realm);
    st.handles.retain(|(r, _)| *r != realm);
    st.resolvers.retain(|(r, _)| *r != realm);
}

/// After JS ran: clear a pending termination and restore the heap limit if
/// the near-heap-limit callback raised it.
fn after_run(iso: &mut v8::OwnedIsolate, state: &SharedState) {
    if iso.is_execution_terminating() {
        iso.cancel_terminate_execution();
    }
    let Some(heap) = state.borrow().heap.clone() else {
        return;
    };
    // A trip not reported through `caught` (e.g. inside a microtask) must
    // not be blamed on the next script.
    heap.tripped.set(false);
    if heap.raised.replace(false) {
        // Re-adding with the original cap lowers the raised limit again.
        iso.remove_near_heap_limit_callback(near_heap_limit, heap.max);
        iso.add_near_heap_limit_callback(near_heap_limit, Rc::as_ptr(&heap) as *mut c_void);
    }
}

/// Realm handle for the [`Realm`] trait.
pub struct RealmRef<'e> {
    engine: &'e mut Engine,
    id: RealmId,
}

impl Realm for RealmRef<'_> {
    fn id(&self) -> RealmId {
        self.id
    }
    fn evaluate(&mut self, source: &str, origin: ScriptOrigin<'_>) -> Result<JsValue, JsError> {
        self.engine.evaluate(self.id, source, origin)
    }
    fn perform_microtask_checkpoint(&mut self) {
        self.engine.perform_microtask_checkpoint();
    }
}

impl JsEngine for Engine {
    type Realm<'e> = RealmRef<'e>;

    fn family(&self) -> BrowserFamily {
        BrowserFamily::Chrome
    }
    fn new_realm(&mut self, options: RealmOptions) -> Result<RealmId, JsError> {
        Engine::new_realm(self, options)
    }
    fn realm(&mut self, id: RealmId) -> Option<RealmRef<'_>> {
        if self.state.borrow().realms.contains_key(&id) {
            Some(RealmRef { engine: self, id })
        } else {
            None
        }
    }
    fn drop_realm(&mut self, id: RealmId) {
        Engine::drop_realm(self, id);
    }
    fn perform_microtask_checkpoint(&mut self) {
        Engine::perform_microtask_checkpoint(self);
    }
    fn low_memory_notification(&mut self) {
        Engine::low_memory_notification(self);
    }
    fn heap_stats(&mut self) -> HeapStats {
        Engine::heap_stats(self)
    }
}

// ---------------------------------------------------------------------------
// Callback contexts
// ---------------------------------------------------------------------------

/// What a [`HostFn`] sees: arguments, `this`, the return slot.
pub struct CallCtx<'a, 's, 'i> {
    scope: &'a mut v8::PinScope<'s, 'i>,
    args: v8::FunctionCallbackArguments<'s>,
    rv: v8::ReturnValue<'s, v8::Value>,
    state: SharedState,
    realm: RealmId,
}

impl<'s, 'i> CallCtx<'_, 's, 'i> {
    /// Number of arguments passed.
    pub fn len(&self) -> usize {
        self.args.length().max(0) as usize
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// The realm the call happens in.
    pub fn realm(&self) -> RealmId {
        self.realm
    }
    /// Called with `new`.
    pub fn is_construct_call(&self) -> bool {
        self.args.is_construct_call()
    }

    fn arg(&self, i: usize) -> v8::Local<'s, v8::Value> {
        self.args.get(i.min(i32::MAX as usize) as i32)
    }

    pub fn arg_kind(&self, i: usize) -> ValueKind {
        value_kind(self.arg(i))
    }
    /// ToBoolean.
    pub fn arg_bool(&mut self, i: usize) -> bool {
        self.arg(i).boolean_value(self.scope)
    }
    /// ToNumber (may run `valueOf` and throw).
    pub fn arg_f64(&mut self, i: usize) -> Result<f64, HostError> {
        self.arg(i)
            .number_value(self.scope)
            .ok_or(HostError::Thrown)
    }
    /// ToInt32 (WebIDL `long` without `[EnforceRange]`).
    pub fn arg_i32(&mut self, i: usize) -> Result<i32, HostError> {
        self.arg(i).int32_value(self.scope).ok_or(HostError::Thrown)
    }
    /// ToUint32 (WebIDL `unsigned long`).
    pub fn arg_u32(&mut self, i: usize) -> Result<u32, HostError> {
        self.arg(i)
            .uint32_value(self.scope)
            .ok_or(HostError::Thrown)
    }
    /// ToString (`DOMString`); symbols throw like in Chrome.
    pub fn arg_string(&mut self, i: usize) -> Result<String, HostError> {
        let v = self.arg(i);
        let s = v.to_string(self.scope).ok_or(HostError::Thrown)?;
        Ok(s.to_rust_string_lossy(self.scope))
    }
    /// The host object behind an argument, if it is a wrapper.
    pub fn arg_host(&mut self, i: usize) -> Option<HostRef> {
        let v = self.arg(i);
        v8::Local::<v8::Object>::try_from(v)
            .ok()
            .and_then(|o| read_host(self.scope, o))
    }
    /// Keep an argument (e.g. a listener) as a handle.
    pub fn arg_handle(&mut self, i: usize) -> Handle {
        let v = self.arg(i);
        store_handle(self.scope, &self.state, self.realm, v)
    }
    /// Copy an argument out (objects as JSON; may run `toJSON`).
    pub fn arg_value(&mut self, i: usize) -> JsValue {
        let v = self.arg(i);
        to_js_value(self.scope, v)
    }

    /// The host object behind `this`, if it is a wrapper (or the global).
    pub fn this_host(&mut self) -> Option<HostRef> {
        let this = self.args.this();
        read_host(self.scope, this)
    }

    /// In a constructor: bind the new object to `host` and cache it as
    /// the wrapper, so later wraps of `host` return this object.
    pub fn init_this(&mut self, host: HostRef) -> Result<(), HostError> {
        let this = self.args.this();
        if this.internal_field_count() < HOST_FIELDS {
            return Err(HostError::type_error("Illegal invocation"));
        }
        set_host(self.scope, this, host);
        let global = v8::Global::new(self.scope, this);
        self.state
            .borrow_mut()
            .wrappers
            .insert((self.realm, host), global);
        Ok(())
    }

    /// Return `value` from a constructor that makes its object itself (a
    /// legacy factory function such as `Image`): when `new.target` gave
    /// `this` another prototype (`Reflect.construct(Image, [], F)`, a
    /// subclass), the returned object gets that prototype, as in Chrome.
    pub fn ret_constructed(&mut self, value: impl Into<Arg>) -> Result<(), HostError> {
        let v = arg_to_v8(self.scope, &self.state, self.realm, &value.into())?;
        if self.args.is_construct_call()
            && let Ok(obj) = v8::Local::<v8::Object>::try_from(v)
            && let Some(want) = self.args.this().get_prototype(self.scope)
            && obj.get_prototype(self.scope) != Some(want)
        {
            obj.set_prototype(self.scope, want);
        }
        self.rv.set(v);
        Ok(())
    }

    /// Set the return value.
    pub fn ret(&mut self, value: impl Into<Arg>) -> Result<(), HostError> {
        let v = arg_to_v8(self.scope, &self.state, self.realm, &value.into())?;
        self.rv.set(v);
        Ok(())
    }

    /// Return a new pending promise; settle it later through the resolver.
    pub fn ret_new_promise(&mut self) -> Result<Resolver, HostError> {
        let (promise, resolver) =
            new_promise(self.scope, &self.state, self.realm).ok_or(HostError::Thrown)?;
        self.rv.set(promise.into());
        Ok(resolver)
    }

    /// A new pending promise held by the host (e.g. a cached `ready`
    /// promise), with its resolver.
    pub fn new_promise(&mut self) -> Result<(Handle, Resolver), HostError> {
        let (promise, resolver) =
            new_promise(self.scope, &self.state, self.realm).ok_or(HostError::Thrown)?;
        Ok((
            store_handle(self.scope, &self.state, self.realm, promise.into()),
            resolver,
        ))
    }

    /// Settle a promise from inside a callback.
    pub fn resolve_promise(&mut self, resolver: Resolver, value: Arg) -> Result<(), HostError> {
        settle_promise(self.scope, &self.state, self.realm, resolver, Ok(value))
    }

    /// Reject a promise from inside a callback.
    pub fn reject_promise(
        &mut self,
        resolver: Resolver,
        error: HostError,
    ) -> Result<(), HostError> {
        settle_promise(self.scope, &self.state, self.realm, resolver, Err(error))
    }

    /// Call a held function (e.g. a `forEach` callback). An exception
    /// propagates as [`HostError::Thrown`].
    pub fn call(&mut self, func: Handle, this: Arg, args: &[Arg]) -> Result<JsValue, HostError> {
        let realm = self.realm;
        match call_inner(self.scope, &self.state, realm, func, &this, args) {
            Ok(Some(v)) => Ok(to_js_value(self.scope, v)),
            Ok(None) => Err(HostError::Thrown),
            Err(e) => Err(HostError::Error(e.message)),
        }
    }

    /// Wrap a host object without returning it (e.g. to store a handle).
    pub fn wrap(&mut self, iface: InterfaceId, host: HostRef) -> Result<Handle, HostError> {
        let obj = wrap_host(self.scope, &self.state, self.realm, iface, host)
            .map_err(|e| HostError::Error(e.message))?;
        Ok(store_handle(
            self.scope,
            &self.state,
            self.realm,
            obj.into(),
        ))
    }

    /// Store a value on `this` under a private key that JS cannot see
    /// (per-object host state such as a `DOMException`'s name). Use
    /// qualified keys (`"DOMException#name"`); keys are never collected.
    pub fn set_this_private(&mut self, key: &str, value: impl Into<Arg>) -> Result<(), HostError> {
        let v = arg_to_v8(self.scope, &self.state, self.realm, &value.into())?;
        let k = v8_str(self.scope, key);
        let p = v8::Private::for_api(self.scope, Some(k));
        self.args.this().set_private(self.scope, p, v);
        Ok(())
    }

    /// Read a value stored with [`CallCtx::set_this_private`]; `Undefined`
    /// if unset.
    pub fn this_private(&mut self, key: &str) -> JsValue {
        let k = v8_str(self.scope, key);
        let p = v8::Private::for_api(self.scope, Some(k));
        match self.args.this().get_private(self.scope, p) {
            Some(v) => to_js_value(self.scope, v),
            None => JsValue::Undefined,
        }
    }

    /// Call a held function and catch what it throws: the exception comes
    /// back as a [`JsError`] instead of propagating (event listeners,
    /// callbacks whose errors are "reported"). A termination still unwinds.
    pub fn try_call(&mut self, func: Handle, this: Arg, args: &[Arg]) -> Result<JsValue, JsError> {
        let realm = self.realm;
        let state = self.state.clone();
        v8::tc_scope!(let tc, self.scope);
        match call_inner(tc, &state, realm, func, &this, args)? {
            Some(v) => Ok(to_js_value(tc, v)),
            None => Err(caught_nested(tc)),
        }
    }

    /// Like [`CallCtx::try_call`], keeping the result as a handle.
    pub fn try_call_handle(
        &mut self,
        func: Handle,
        this: Arg,
        args: &[Arg],
    ) -> Result<Handle, JsError> {
        let realm = self.realm;
        let state = self.state.clone();
        v8::tc_scope!(let tc, self.scope);
        match call_inner(tc, &state, realm, func, &this, args)? {
            Some(v) => Ok(store_handle(tc, &state, realm, v)),
            None => Err(caught_nested(tc)),
        }
    }

    /// Compile and run a classic script in the current realm from inside a
    /// callback (a script element inserted by script). Exceptions are
    /// caught and returned.
    pub fn try_evaluate(
        &mut self,
        source: &str,
        origin: ScriptOrigin<'_>,
    ) -> Result<JsValue, JsError> {
        v8::tc_scope!(let tc, self.scope);
        match run_script(tc, source, origin) {
            Some(v) => Ok(to_js_value(tc, v)),
            None => Err(caught_nested(tc)),
        }
    }

    /// Like [`CallCtx::try_evaluate`], keeping the result as a handle.
    pub fn try_evaluate_handle(
        &mut self,
        source: &str,
        origin: ScriptOrigin<'_>,
    ) -> Result<Handle, JsError> {
        let realm = self.realm;
        let state = self.state.clone();
        v8::tc_scope!(let tc, self.scope);
        match run_script(tc, source, origin) {
            Some(v) => Ok(store_handle(tc, &state, realm, v)),
            None => Err(caught_nested(tc)),
        }
    }

    /// `this` as a handle.
    pub fn this_handle(&mut self) -> Handle {
        let this: v8::Local<v8::Value> = self.args.this().into();
        store_handle(self.scope, &self.state, self.realm, this)
    }

    /// `obj[key]` of a held value as a new handle (`undefined` for
    /// non-objects). Getters run; an exception propagates as
    /// [`HostError::Thrown`].
    pub fn get_property(&mut self, obj: Handle, key: &str) -> Result<Handle, HostError> {
        let v =
            load_handle(self.scope, &self.state, obj).map_err(|e| HostError::Error(e.message))?;
        let out = match v8::Local::<v8::Object>::try_from(v) {
            Ok(o) => {
                let k = v8_str(self.scope, key);
                o.get(self.scope, k.into()).ok_or(HostError::Thrown)?
            }
            Err(_) => v8::undefined(self.scope).into(),
        };
        Ok(store_handle(self.scope, &self.state, self.realm, out))
    }

    /// `obj[key] = value` on a held object. No-op for non-objects.
    pub fn set_property(&mut self, obj: Handle, key: &str, value: Arg) -> Result<(), HostError> {
        let v =
            load_handle(self.scope, &self.state, obj).map_err(|e| HostError::Error(e.message))?;
        if let Ok(o) = v8::Local::<v8::Object>::try_from(v) {
            let val = arg_to_v8(self.scope, &self.state, self.realm, &value)?;
            let k = v8_str(self.scope, key);
            o.set(self.scope, k.into(), val).ok_or(HostError::Thrown)?;
        }
        Ok(())
    }

    /// Materialize an [`Arg`] (e.g. [`Arg::Construct`], [`Arg::Json`]) as a
    /// held value.
    pub fn make_handle(&mut self, value: Arg) -> Result<Handle, HostError> {
        let v = arg_to_v8(self.scope, &self.state, self.realm, &value)?;
        Ok(store_handle(self.scope, &self.state, self.realm, v))
    }

    /// `a === b` for two held values.
    pub fn strict_equals(&mut self, a: Handle, b: Handle) -> bool {
        match (
            load_handle(self.scope, &self.state, a),
            load_handle(self.scope, &self.state, b),
        ) {
            (Ok(x), Ok(y)) => x.strict_equals(y),
            _ => false,
        }
    }

    /// Type of a held value.
    pub fn handle_kind(&mut self, h: Handle) -> ValueKind {
        load_handle(self.scope, &self.state, h).map_or(ValueKind::Undefined, value_kind)
    }

    /// The host object behind a held value, if it is a wrapper.
    pub fn handle_host(&mut self, h: Handle) -> Option<HostRef> {
        let v = load_handle(self.scope, &self.state, h).ok()?;
        v8::Local::<v8::Object>::try_from(v)
            .ok()
            .and_then(|o| read_host(self.scope, o))
    }

    /// Copy a held value out (objects as JSON).
    pub fn handle_value(&mut self, h: Handle) -> JsValue {
        match load_handle(self.scope, &self.state, h) {
            Ok(v) => to_js_value(self.scope, v),
            Err(_) => JsValue::Undefined,
        }
    }

    /// ToString of a held value (may run `toString` and throw).
    pub fn handle_string(&mut self, h: Handle) -> Result<String, HostError> {
        let v = load_handle(self.scope, &self.state, h).map_err(|e| HostError::Error(e.message))?;
        let s = v.to_string(self.scope).ok_or(HostError::Thrown)?;
        Ok(s.to_rust_string_lossy(self.scope))
    }

    /// Bytes of an `ArrayBuffer` / `ArrayBufferView` argument.
    pub fn arg_bytes(&mut self, i: usize) -> Option<Vec<u8>> {
        let v = self.arg(i);
        read_bytes(self.scope, v)
    }

    /// Bytes of a held `ArrayBuffer` / `ArrayBufferView`.
    pub fn handle_bytes(&mut self, h: Handle) -> Option<Vec<u8>> {
        let v = load_handle(self.scope, &self.state, h).ok()?;
        read_bytes(self.scope, v)
    }

    /// Element type of an `ArrayBufferView` argument, `None` for anything
    /// else.
    pub fn arg_view_kind(&mut self, i: usize) -> Option<ViewKind> {
        view_kind(self.arg(i))
    }

    /// Element type of a held `ArrayBufferView`.
    pub fn handle_view_kind(&mut self, h: Handle) -> Option<ViewKind> {
        let v = load_handle(self.scope, &self.state, h).ok()?;
        view_kind(v)
    }

    /// Copy `bytes` into the `ArrayBufferView` argument `i`, starting
    /// `offset` bytes into the view and stopping at its end (typed-array
    /// out-parameters such as `readPixels` or `getFloatFrequencyData`).
    /// Returns the number of bytes written; `None` if the argument is not
    /// a view.
    pub fn write_arg_bytes(&mut self, i: usize, offset: usize, bytes: &[u8]) -> Option<usize> {
        write_view(self.arg(i), offset, bytes)
    }

    /// Like [`CallCtx::write_arg_bytes`] for a held view.
    pub fn write_handle_bytes(&mut self, h: Handle, offset: usize, bytes: &[u8]) -> Option<usize> {
        let v = load_handle(self.scope, &self.state, h).ok()?;
        write_view(v, offset, bytes)
    }

    /// Forget a handle (e.g. a cleared timer's callback).
    pub fn release(&mut self, h: Handle) {
        self.state.borrow_mut().handles.remove(h.idx, h.generation);
    }

    /// Queue a held function as a microtask (`queueMicrotask`).
    pub fn enqueue_microtask(&mut self, func: Handle) -> Result<(), HostError> {
        let v =
            load_handle(self.scope, &self.state, func).map_err(|e| HostError::Error(e.message))?;
        let f = v8::Local::<v8::Function>::try_from(v)
            .map_err(|_| HostError::type_error("not a function"))?;
        self.scope.enqueue_microtask(f);
        Ok(())
    }

    /// Escape hatch: the raw V8 scope.
    pub fn scope(&mut self) -> &mut v8::PinScope<'s, 'i> {
        self.scope
    }
    /// Escape hatch: the raw V8 arguments.
    pub fn v8_args(&self) -> &v8::FunctionCallbackArguments<'s> {
        &self.args
    }
    /// Escape hatch: set a raw V8 return value.
    pub fn ret_v8(&mut self, value: v8::Local<'s, v8::Value>) {
        self.rv.set(value);
    }
}

/// What a [`NamedGetter`] / [`IndexedGetter`] sees.
pub struct PropCtx<'a, 's, 'i> {
    scope: &'a mut v8::PinScope<'s, 'i>,
    args: v8::PropertyCallbackArguments<'s>,
    realm: RealmId,
}

impl<'s, 'i> PropCtx<'_, 's, 'i> {
    pub fn realm(&self) -> RealmId {
        self.realm
    }
    /// The host object that owns the property.
    pub fn holder_host(&mut self) -> Option<HostRef> {
        let holder = self.args.holder();
        read_host(self.scope, holder)
    }
    /// Escape hatch: the raw V8 scope.
    pub fn scope(&mut self) -> &mut v8::PinScope<'s, 'i> {
        self.scope
    }
    /// The realm the holder object was created in. For a window global
    /// read from another frame (`frames[0][0]`) this is the holder's own
    /// realm, while [`PropCtx::realm`] is the caller's.
    pub fn holder_realm(&mut self) -> RealmId {
        let holder = self.args.holder();
        holder
            .get_creation_context(self.scope)
            .and_then(|c| realm_of_context(self.scope, c))
            .unwrap_or(self.realm)
    }
}

// ---------------------------------------------------------------------------
// Trampolines
// ---------------------------------------------------------------------------

fn callback_index(scope: &mut v8::PinScope<'_, '_>, data: v8::Local<'_, v8::Value>) -> usize {
    data.uint32_value(scope).unwrap_or(u32::MAX) as usize
}

fn shared_state(scope: &v8::PinScope<'_, '_>) -> SharedState {
    scope
        .get_slot::<SharedState>()
        .expect("lb-js state slot missing")
        .clone()
}

fn current_realm(scope: &v8::PinScope<'_, '_>) -> RealmId {
    let ctx = scope.get_current_context();
    realm_of_context(scope, ctx).unwrap_or(RealmId(0))
}

/// The realm of a context of this engine, if it is one.
fn realm_of_context(
    scope: &v8::PinScope<'_, '_>,
    ctx: v8::Local<'_, v8::Context>,
) -> Option<RealmId> {
    let state = scope.get_slot::<SharedState>()?;
    let hash = ctx.global(scope).get_identity_hash().get();
    let st = state.try_borrow().ok()?;
    match st.realm_by_global.get(&hash)?.as_slice() {
        [id] => Some(*id),
        ids => ids.iter().copied().find(|id| {
            st.realms
                .get(id)
                .is_some_and(|r| v8::Local::new(scope, &r.context) == ctx)
        }),
    }
}

fn host_trampoline<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    rv: v8::ReturnValue<'s, v8::Value>,
) {
    let state = shared_state(scope);
    let idx = callback_index(scope, args.data());
    let f = match state.borrow().callbacks.get(idx) {
        Some(Callback::Fn(f)) => f.clone(),
        _ => return,
    };
    invoke(scope, args, rv, state, &f);
}

fn ctor_trampoline<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    rv: v8::ReturnValue<'s, v8::Value>,
) {
    let state = shared_state(scope);
    let idx = callback_index(scope, args.data());
    let (iface, f) = match state.borrow().callbacks.get(idx) {
        Some(Callback::Ctor { iface, f }) => (iface.clone(), f.clone()),
        _ => return,
    };
    let realm = current_realm(scope);
    let Some(f) = f else {
        throw(
            scope,
            &state,
            realm,
            HostError::type_error("Illegal constructor"),
        );
        return;
    };
    if !args.is_construct_call() {
        let msg = format!(
            "Failed to construct '{iface}': Please use the 'new' operator, this DOM object constructor cannot be called as a function."
        );
        throw(scope, &state, realm, HostError::TypeError(msg));
        return;
    }
    invoke(scope, args, rv, state, &f);
}

fn invoke<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    rv: v8::ReturnValue<'s, v8::Value>,
    state: SharedState,
    f: &HostFn,
) {
    let realm = current_realm(scope);
    let mut cx = CallCtx {
        scope,
        args,
        rv,
        state,
        realm,
    };
    // A panic must not unwind into V8 (that aborts the process): report it
    // as a JS error instead (builds with `panic = abort` still abort).
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&mut cx)))
        .unwrap_or_else(|_| Err(HostError::Error("internal error in host function".into())));
    if let Err(e) = r {
        let CallCtx { scope, state, .. } = cx;
        throw(scope, &state, realm, e);
    }
}

fn named_trampoline<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    key: v8::Local<'s, v8::Name>,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<v8::Value>,
) -> v8::Intercepted {
    let state = shared_state(scope);
    let idx = named_index(scope, args.data(), 0);
    let f = match state.borrow().callbacks.get(idx) {
        Some(Callback::Named(f)) => f.clone(),
        _ => return v8::Intercepted::kNo,
    };
    let name = key.to_rust_string_lossy(scope);
    let realm = current_realm(scope);
    let mut cx = PropCtx { scope, args, realm };
    let result = f(&mut cx, &name);
    finish_interceptor(cx.scope, &state, realm, result, &mut rv)
}

/// Callback index `slot` (0 get, 1 set, 2 delete, 3 enum) of a named
/// handler whose data is the getter's index.
fn named_index(
    scope: &mut v8::PinScope<'_, '_>,
    data: v8::Local<'_, v8::Value>,
    slot: u32,
) -> usize {
    callback_index(scope, data).saturating_add(slot as usize)
}

fn named_set_trampoline<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    key: v8::Local<'s, v8::Name>,
    value: v8::Local<'s, v8::Value>,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<v8::Boolean>,
) -> v8::Intercepted {
    let state = shared_state(scope);
    let idx = named_index(scope, args.data(), 1);
    let f = match state.borrow().callbacks.get(idx) {
        Some(Callback::NamedSet(f)) => f.clone(),
        _ => return v8::Intercepted::kNo,
    };
    let name = key.to_rust_string_lossy(scope);
    let realm = current_realm(scope);
    let Some(value) = value.to_string(scope) else {
        return v8::Intercepted::kYes;
    };
    let value = value.to_rust_string_lossy(scope);
    let mut cx = PropCtx { scope, args, realm };
    match f(&mut cx, &name, &value) {
        Ok(false) => v8::Intercepted::kNo,
        Ok(true) => {
            rv.set_bool(true);
            v8::Intercepted::kYes
        }
        Err(e) => {
            throw(cx.scope, &state, realm, e);
            v8::Intercepted::kYes
        }
    }
}

fn named_delete_trampoline<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    key: v8::Local<'s, v8::Name>,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<v8::Boolean>,
) -> v8::Intercepted {
    let state = shared_state(scope);
    let idx = named_index(scope, args.data(), 2);
    let f = match state.borrow().callbacks.get(idx) {
        Some(Callback::NamedDelete(f)) => f.clone(),
        _ => return v8::Intercepted::kNo,
    };
    let name = key.to_rust_string_lossy(scope);
    let realm = current_realm(scope);
    let mut cx = PropCtx { scope, args, realm };
    match f(&mut cx, &name) {
        Ok(None) => v8::Intercepted::kNo,
        Ok(Some(b)) => {
            rv.set_bool(b);
            v8::Intercepted::kYes
        }
        Err(e) => {
            throw(cx.scope, &state, realm, e);
            v8::Intercepted::kYes
        }
    }
}

fn named_enum_trampoline<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<v8::Array>,
) {
    let state = shared_state(scope);
    let idx = named_index(scope, args.data(), 3);
    let f = match state.borrow().callbacks.get(idx) {
        Some(Callback::NamedEnum(f)) => f.clone(),
        _ => return,
    };
    let realm = current_realm(scope);
    let mut cx = PropCtx { scope, args, realm };
    let names = f(&mut cx);
    let scope = cx.scope;
    let elems: Vec<v8::Local<v8::Value>> = names.iter().map(|n| v8_str(scope, n).into()).collect();
    rv.set(v8::Array::new_with_elements(scope, &elems));
}

/// `[LegacyUnenumerableNamedProperties]`: supported names are read-only
/// and not enumerable.
fn named_query_trampoline<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    key: v8::Local<'s, v8::Name>,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<v8::Integer>,
) -> v8::Intercepted {
    let state = shared_state(scope);
    let idx = named_index(scope, args.data(), 0);
    let f = match state.borrow().callbacks.get(idx) {
        Some(Callback::Named(f)) => f.clone(),
        _ => return v8::Intercepted::kNo,
    };
    let name = key.to_rust_string_lossy(scope);
    let realm = current_realm(scope);
    let mut cx = PropCtx { scope, args, realm };
    match f(&mut cx, &name) {
        Ok(Some(_)) => {
            let attrs = v8::PropertyAttribute::READ_ONLY | v8::PropertyAttribute::DONT_ENUM;
            rv.set_int32(attrs.as_u32() as i32);
            v8::Intercepted::kYes
        }
        _ => v8::Intercepted::kNo,
    }
}

/// Supported indices of an interface with [`IndexedLength`].
fn indexed_len<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::PropertyCallbackArguments<'s>,
) -> Option<u32> {
    let state = shared_state(scope);
    let idx = callback_index(scope, args.data()).saturating_add(1);
    let f = match state.borrow().callbacks.get(idx) {
        Some(Callback::IndexedLen(f)) => f.clone(),
        _ => return None,
    };
    let realm = current_realm(scope);
    let mut cx = PropCtx { scope, args, realm };
    Some(f(&mut cx))
}

fn indexed_query_trampoline<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    index: u32,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<v8::Integer>,
) -> v8::Intercepted {
    match indexed_len(scope, args) {
        Some(n) if index < n => {
            rv.set_int32(v8::PropertyAttribute::READ_ONLY.as_u32() as i32);
            v8::Intercepted::kYes
        }
        _ => v8::Intercepted::kNo,
    }
}

fn indexed_enum_trampoline<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<v8::Array>,
) {
    let n = indexed_len(scope, args).unwrap_or(0);
    let elems: Vec<v8::Local<v8::Value>> = (0..n)
        .map(|i| v8::Integer::new_from_unsigned(scope, i).into())
        .collect();
    rv.set(v8::Array::new_with_elements(scope, &elems));
}

/// Read-only indexed properties: assignment is ignored (Blink, sloppy
/// mode).
fn indexed_set_trampoline<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    index: u32,
    _value: v8::Local<'s, v8::Value>,
    args: v8::PropertyCallbackArguments<'s>,
    _rv: v8::ReturnValue<v8::Boolean>,
) -> v8::Intercepted {
    match indexed_len(scope, args) {
        Some(n) if index < n => v8::Intercepted::kYes,
        _ => v8::Intercepted::kNo,
    }
}

fn indexed_delete_trampoline<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    index: u32,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<v8::Boolean>,
) -> v8::Intercepted {
    match indexed_len(scope, args) {
        Some(n) if index < n => {
            rv.set_bool(false);
            v8::Intercepted::kYes
        }
        _ => v8::Intercepted::kNo,
    }
}

fn indexed_trampoline<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    index: u32,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<v8::Value>,
) -> v8::Intercepted {
    let state = shared_state(scope);
    let idx = callback_index(scope, args.data());
    let f = match state.borrow().callbacks.get(idx) {
        Some(Callback::Indexed(f)) => f.clone(),
        _ => return v8::Intercepted::kNo,
    };
    let realm = current_realm(scope);
    let mut cx = PropCtx { scope, args, realm };
    let result = f(&mut cx, index);
    finish_interceptor(cx.scope, &state, realm, result, &mut rv)
}

fn finish_interceptor(
    scope: &mut v8::PinScope<'_, '_>,
    state: &SharedState,
    realm: RealmId,
    result: Result<Option<Arg>, HostError>,
    rv: &mut v8::ReturnValue<v8::Value>,
) -> v8::Intercepted {
    match result {
        Ok(None) => v8::Intercepted::kNo,
        Ok(Some(arg)) => match arg_to_v8(scope, state, realm, &arg) {
            Ok(v) => {
                rv.set(v);
                v8::Intercepted::kYes
            }
            Err(e) => {
                throw(scope, state, realm, e);
                v8::Intercepted::kYes
            }
        },
        Err(e) => {
            throw(scope, state, realm, e);
            v8::Intercepted::kYes
        }
    }
}

// ---------------------------------------------------------------------------
// Templates
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Target<'s> {
    Function(v8::Local<'s, v8::FunctionTemplate>),
    Object(v8::Local<'s, v8::ObjectTemplate>),
}

fn v8_attrs(attrs: PropAttrs, accessor: bool) -> v8::PropertyAttribute {
    let mut a = v8::PropertyAttribute::NONE;
    if !attrs.writable && !accessor {
        a = a | v8::PropertyAttribute::READ_ONLY;
    }
    if !attrs.enumerable {
        a = a | v8::PropertyAttribute::DONT_ENUM;
    }
    if !attrs.configurable {
        a = a | v8::PropertyAttribute::DONT_DELETE;
    }
    a
}

fn v8_key<'s>(scope: &v8::PinScope<'s, '_, ()>, key: &PropKey) -> v8::Local<'s, v8::Name> {
    match key {
        PropKey::Name(n) => v8_str(scope, n).into(),
        PropKey::Symbol(s) => match s {
            WellKnownSymbol::Iterator => v8::Symbol::get_iterator(scope),
            WellKnownSymbol::AsyncIterator => v8::Symbol::get_async_iterator(scope),
            WellKnownSymbol::ToStringTag => v8::Symbol::get_to_string_tag(scope),
            WellKnownSymbol::ToPrimitive => v8::Symbol::get_to_primitive(scope),
            WellKnownSymbol::Unscopables => v8::Symbol::get_unscopables(scope),
        }
        .into(),
    }
}

/// Function name V8 should show: the key, or `[Symbol.x]` for symbols.
fn function_name(key: &PropKey, prefix: &str) -> String {
    let base = match key {
        PropKey::Name(n) => n.clone(),
        PropKey::Symbol(s) => match s {
            WellKnownSymbol::Iterator => "[Symbol.iterator]",
            WellKnownSymbol::AsyncIterator => "[Symbol.asyncIterator]",
            WellKnownSymbol::ToStringTag => "[Symbol.toStringTag]",
            WellKnownSymbol::ToPrimitive => "[Symbol.toPrimitive]",
            WellKnownSymbol::Unscopables => "[Symbol.unscopables]",
        }
        .to_owned(),
    };
    format!("{prefix}{base}")
}

fn function_template<'s>(
    scope: &mut v8::PinScope<'s, '_, ()>,
    st: &mut State,
    f: &HostFn,
    length: u32,
    name: &str,
    signature: Option<v8::Local<'s, v8::Signature>>,
) -> v8::Local<'s, v8::FunctionTemplate> {
    let idx = st.push_callback(Callback::Fn(f.clone()));
    let data = v8::Integer::new_from_unsigned(scope, idx);
    let mut b = v8::FunctionTemplate::builder(host_trampoline)
        .data(data.into())
        .length(length as i32)
        .constructor_behavior(v8::ConstructorBehavior::Throw);
    if let Some(sig) = signature {
        b = b.signature(sig);
    }
    let ft = b.build(scope);
    ft.set_class_name(v8_str(scope, name));
    ft
}

/// Install one member. `methods` collects the method templates of this
/// member list so later [`MemberKind::Alias`] members can reuse them.
fn install_member<'s>(
    scope: &mut v8::PinScope<'s, '_, ()>,
    st: &mut State,
    target: Target<'s>,
    signature: Option<v8::Local<'s, v8::Signature>>,
    m: &Member,
    methods: &mut Vec<(PropKey, v8::Local<'s, v8::FunctionTemplate>)>,
    this_iface: (InterfaceId, v8::Local<'s, v8::FunctionTemplate>),
) -> Option<Made<'s>> {
    let key = v8_key(scope, &m.key);
    let template: &v8::Template = match &target {
        Target::Function(ft) => ft,
        Target::Object(ot) => ot,
    };
    match &m.kind {
        MemberKind::Method { length, f } => {
            let name = function_name(&m.key, "");
            let ft = function_template(scope, st, f, *length, &name, signature);
            template.set_with_attr(key, ft.into(), v8_attrs(m.attrs, false));
            methods.push((m.key.clone(), ft));
            Some(Made::Method(ft))
        }
        MemberKind::Alias(of) => {
            // A function template is instantiated once per context, so the
            // same template under two keys gives the same function object.
            let (_, ft) = *methods.iter().find(|(k, _)| k == of)?;
            template.set_with_attr(key, ft.into(), v8_attrs(m.attrs, false));
            Some(Made::Method(ft))
        }
        MemberKind::Namespace {
            to_string_tag,
            members,
        } => {
            let ot = v8::ObjectTemplate::new(scope);
            let mut inner = Vec::new();
            for member in members {
                install_member(
                    scope,
                    st,
                    Target::Object(ot),
                    None,
                    member,
                    &mut inner,
                    this_iface,
                );
            }
            let tag = v8::Symbol::get_to_string_tag(scope);
            let value = v8_str(scope, to_string_tag);
            ot.set_with_attr(
                tag.into(),
                value.into(),
                v8::PropertyAttribute::READ_ONLY | v8::PropertyAttribute::DONT_ENUM,
            );
            template.set_with_attr(key, ot.into(), v8_attrs(m.attrs, false));
            None
        }
        MemberKind::Accessor { get, set } => {
            let getter = get.as_ref().map(|f| {
                let name = function_name(&m.key, "get ");
                function_template(scope, st, f, 0, &name, signature)
            });
            let setter = set.as_ref().map(|f| {
                let name = function_name(&m.key, "set ");
                function_template(scope, st, f, 1, &name, signature)
            });
            if getter.is_none() && setter.is_none() {
                return None;
            }
            let attrs = v8_attrs(m.attrs, true);
            match target {
                Target::Function(ft) => ft.set_accessor_property(key, getter, setter, attrs),
                Target::Object(ot) => ot.set_accessor_property(key, getter, setter, attrs),
            }
            Some(Made::Accessor(getter, setter))
        }
        MemberKind::Value(v) => {
            let value: v8::Local<v8::Data> = match v {
                JsValue::Undefined => v8::undefined(scope).into(),
                JsValue::Null => v8::null(scope).into(),
                JsValue::Bool(b) => v8::Boolean::new(scope, *b).into(),
                JsValue::Number(n) => v8::Number::new(scope, *n).into(),
                JsValue::String(s) | JsValue::Json(s) | JsValue::Opaque(s) => {
                    v8_str(scope, s).into()
                }
            };
            template.set_with_attr(key, value, v8_attrs(m.attrs, false));
            Some(Made::Value)
        }
        MemberKind::Interface(id) if *id == this_iface.0 => {
            // The interface being defined (the global's own interface object).
            template.set_with_attr(key, this_iface.1.into(), v8_attrs(m.attrs, false));
            None
        }
        MemberKind::Interface(id) => {
            if let Some(entry) = st.entry(*id) {
                let ft = v8::Local::new(scope, &entry.template);
                template.set_with_attr(key, ft.into(), v8_attrs(m.attrs, false));
            }
            None
        }
        MemberKind::Intrinsic(which) => {
            let intrinsic = match which {
                Intrinsic::ArrayProtoEntries => v8::Intrinsic::ArrayProtoEntries,
                Intrinsic::ArrayProtoForEach => v8::Intrinsic::ArrayProtoForEach,
                Intrinsic::ArrayProtoKeys => v8::Intrinsic::ArrayProtoKeys,
                Intrinsic::ArrayProtoValues => v8::Intrinsic::ArrayProtoValues,
            };
            template.set_intrinsic_data_property(key, intrinsic, v8_attrs(m.attrs, false));
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Values, handles, wrappers
// ---------------------------------------------------------------------------

/// A primitive [`JsValue`] as a V8 value (`Json` and `Opaque` as strings,
/// as on templates).
fn js_to_v8<'s>(scope: &v8::PinScope<'s, '_, ()>, v: &JsValue) -> v8::Local<'s, v8::Value> {
    match v {
        JsValue::Undefined => v8::undefined(scope).into(),
        JsValue::Null => v8::null(scope).into(),
        JsValue::Bool(b) => v8::Boolean::new(scope, *b).into(),
        JsValue::Number(n) => v8::Number::new(scope, *n).into(),
        JsValue::String(s) | JsValue::Json(s) | JsValue::Opaque(s) => v8_str(scope, s).into(),
    }
}

fn v8_str<'s>(scope: &v8::PinScope<'s, '_, ()>, s: &str) -> v8::Local<'s, v8::String> {
    // Only fails above V8's max string length (~512 MB).
    v8::String::new(scope, s).unwrap_or_else(|| v8::String::empty(scope))
}

fn value_kind(v: v8::Local<'_, v8::Value>) -> ValueKind {
    if v.is_undefined() {
        ValueKind::Undefined
    } else if v.is_null() {
        ValueKind::Null
    } else if v.is_boolean() {
        ValueKind::Bool
    } else if v.is_number() {
        ValueKind::Number
    } else if v.is_big_int() {
        ValueKind::BigInt
    } else if v.is_string() {
        ValueKind::String
    } else if v.is_symbol() {
        ValueKind::Symbol
    } else if v.is_function() {
        ValueKind::Function
    } else {
        ValueKind::Object
    }
}

fn detail_string(scope: &mut v8::PinScope<'_, '_>, v: v8::Local<'_, v8::Value>) -> String {
    v8::tc_scope!(let tc, scope);
    v.to_detail_string(tc)
        .map(|s| s.to_rust_string_lossy(tc))
        .unwrap_or_default()
}

fn to_js_value(scope: &mut v8::PinScope<'_, '_>, v: v8::Local<'_, v8::Value>) -> JsValue {
    match value_kind(v) {
        ValueKind::Undefined => JsValue::Undefined,
        ValueKind::Null => JsValue::Null,
        ValueKind::Bool => JsValue::Bool(v.is_true()),
        ValueKind::Number => JsValue::Number(v.number_value(scope).unwrap_or(f64::NAN)),
        ValueKind::String => JsValue::String(v.to_rust_string_lossy(scope)),
        ValueKind::BigInt | ValueKind::Symbol | ValueKind::Function => {
            JsValue::Opaque(detail_string(scope, v))
        }
        ValueKind::Object => {
            let json = {
                v8::tc_scope!(let tc, scope);
                v8::json::stringify(tc, v).map(|s| s.to_rust_string_lossy(tc))
            };
            match json {
                Some(s) if s != "undefined" => JsValue::Json(s),
                _ => JsValue::Opaque(detail_string(scope, v)),
            }
        }
    }
}

fn store_handle(
    scope: &mut v8::PinScope<'_, '_>,
    state: &SharedState,
    realm: RealmId,
    v: v8::Local<'_, v8::Value>,
) -> Handle {
    let global = v8::Global::new(scope, v);
    let (idx, generation) = state.borrow_mut().handles.insert((realm, global));
    Handle { idx, generation }
}

fn load_handle<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    state: &SharedState,
    handle: Handle,
) -> Result<v8::Local<'s, v8::Value>, JsError> {
    let st = state.borrow();
    let (_, global) = st
        .handles
        .get(handle.idx, handle.generation)
        .ok_or_else(|| JsError::host("stale handle"))?;
    Ok(v8::Local::new(scope, global))
}

fn set_host(scope: &mut v8::PinScope<'_, '_>, obj: v8::Local<'_, v8::Object>, host: HostRef) {
    let kind = v8::Integer::new_from_unsigned(scope, host.kind);
    let id = v8::Integer::new_from_unsigned(scope, host.id);
    obj.set_internal_field(0, kind.into());
    obj.set_internal_field(1, id.into());
}

fn read_host(scope: &mut v8::PinScope<'_, '_>, obj: v8::Local<'_, v8::Object>) -> Option<HostRef> {
    if obj.internal_field_count() < HOST_FIELDS {
        return None;
    }
    let field = |i| -> Option<u32> {
        let data = obj.get_internal_field(scope, i)?;
        let int = v8::Local::<v8::Integer>::try_from(data).ok()?;
        u32::try_from(int.value()).ok()
    };
    Some(HostRef {
        kind: field(0)?,
        id: field(1)?,
    })
}

fn wrap_host<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    state: &SharedState,
    realm: RealmId,
    iface: InterfaceId,
    host: HostRef,
) -> Result<v8::Local<'s, v8::Object>, JsError> {
    let mat = {
        let st = state.borrow();
        if let Some(g) = st.wrappers.get(&(realm, host)) {
            return Ok(v8::Local::new(scope, g));
        }
        st.mat(realm)
    };
    // The interface object first, so the wrapper's prototype chain is the
    // realm's fixed one.
    if let Some(mat) = &mat {
        interface_object(scope, state, mat, iface);
    }
    let ft = ensure_template(scope, state, iface)
        .ok_or_else(|| JsError::host(format!("unknown interface {iface:?}")))?;
    let obj = ft
        .instance_template(scope)
        .new_instance(scope)
        .ok_or_else(|| JsError::host("wrapper creation failed"))?;
    set_host(scope, obj, host);
    apply_overflow(scope, state, iface, obj);
    let global = v8::Global::new(scope, obj);
    state.borrow_mut().wrappers.insert((realm, host), global);
    Ok(obj)
}

/// Define an interface's overflow members (those not on its instance
/// template, see [`MAX_TEMPLATE_PROPS`]) on a fresh instance, in member
/// order: interface objects directly, the rest with the descriptors they
/// get on their chunk template's instance.
fn apply_overflow<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    state: &SharedState,
    iface: InterfaceId,
    obj: v8::Local<'s, v8::Object>,
) {
    let _ = apply_overflow_with(scope, state, iface, obj, &[]);
}

/// [`apply_overflow`], with the keys of `last` put at the end, in that
/// order: members among them are skipped in member order and defined
/// there, other existing own properties (engine built-ins) are moved
/// there. The same result as defining everything and then moving `last`,
/// without defining twice.
fn apply_overflow_with<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    state: &SharedState,
    iface: InterfaceId,
    obj: v8::Local<'s, v8::Object>,
    last: &[PropKey],
) -> Result<(), JsError> {
    /// [`Direct`] with local handles.
    enum DirectL<'s> {
        Method(v8::Local<'s, v8::FunctionTemplate>, PropAttrs),
        Accessor(
            Option<v8::Local<'s, v8::FunctionTemplate>>,
            Option<v8::Local<'s, v8::FunctionTemplate>>,
            PropAttrs,
        ),
        Value(JsValue, PropAttrs),
    }
    #[allow(clippy::type_complexity)]
    let (chunks, items, direct): (
        Vec<v8::Local<'s, v8::ObjectTemplate>>,
        Vec<OverflowItem>,
        Vec<Option<DirectL<'s>>>,
    ) = {
        let st = state.borrow();
        let Some(entry) = st.entry(iface) else {
            return Ok(());
        };
        let o = &entry.overflow;
        let chunks = o.chunks.iter().map(|t| v8::Local::new(scope, t)).collect();
        let l = |g: &v8::Global<v8::FunctionTemplate>| v8::Local::new(scope, g);
        let direct = o
            .direct
            .iter()
            .map(|d| {
                Some(match d.as_ref()? {
                    Direct::Method(ft, a) => DirectL::Method(l(ft), *a),
                    Direct::Accessor(g, s, a) => {
                        DirectL::Accessor(g.as_ref().map(l), s.as_ref().map(l), *a)
                    }
                    Direct::Value(v, a) => DirectL::Value(v.clone(), *a),
                })
            })
            .collect();
        (chunks, o.items.clone(), direct)
    };
    let field = |scope: &mut v8::PinScope<'s, '_>, name: &str| -> v8::Local<'s, v8::Value> {
        v8_str(scope, name).into()
    };
    let fields = [
        field(scope, "enumerable"),
        field(scope, "configurable"),
        field(scope, "get"),
        field(scope, "set"),
        field(scope, "value"),
        field(scope, "writable"),
    ];
    let item_key = |item: &OverflowItem| match item {
        OverflowItem::Interface { key, .. } | OverflowItem::Chunk { key, .. } => key.clone(),
    };
    let deferred: HashMap<PropKey, usize> = if last.is_empty() {
        HashMap::new()
    } else {
        let wanted: std::collections::HashSet<&PropKey> = last.iter().collect();
        items
            .iter()
            .enumerate()
            .filter(|(_, it)| wanted.contains(&item_key(it)))
            .map(|(i, it)| (item_key(it), i))
            .collect()
    };
    let mut tmps: Vec<Option<v8::Local<'s, v8::Object>>> = vec![None; chunks.len()];
    let mut define = |scope: &mut v8::PinScope<'s, '_>, i: usize| match &items[i] {
        OverflowItem::Interface { key, id, attrs } => {
            let k = v8_key(scope, key);
            define_lazy_interface(scope, obj, k, *id, *attrs);
        }
        OverflowItem::Chunk { key, .. } if direct.get(i).is_some_and(Option::is_some) => {
            let k = v8_key(scope, key);
            match &direct[i] {
                Some(DirectL::Method(ft, a)) => {
                    if let Some(f) = ft.get_function(scope) {
                        obj.define_own_property(scope, k, f.into(), v8_attrs(*a, false));
                    }
                }
                Some(DirectL::Accessor(g, s, a)) => {
                    let undefined: v8::Local<v8::Value> = v8::undefined(scope).into();
                    let get = g
                        .and_then(|ft| ft.get_function(scope))
                        .map_or(undefined, Into::into);
                    let set = s
                        .and_then(|ft| ft.get_function(scope))
                        .map_or(undefined, Into::into);
                    let mut desc = v8::PropertyDescriptor::new_from_get_set(get, set);
                    desc.set_enumerable(a.enumerable);
                    desc.set_configurable(a.configurable);
                    obj.define_property(scope, k, &desc);
                }
                Some(DirectL::Value(v, a)) => {
                    let value = js_to_v8(scope, v);
                    obj.define_own_property(scope, k, value, v8_attrs(*a, false));
                }
                None => {}
            }
        }
        OverflowItem::Chunk { key, chunk } => {
            let tmp = match tmps[*chunk] {
                Some(t) => t,
                None => {
                    let Some(t) = chunks[*chunk].new_instance(scope) else {
                        return;
                    };
                    tmps[*chunk] = Some(t);
                    t
                }
            };
            let k = v8_key(scope, key);
            let Some(d) = tmp.get_own_property_descriptor(scope, k) else {
                return;
            };
            let Ok(d) = v8::Local::<v8::Object>::try_from(d) else {
                return;
            };
            let desc = descriptor_with(scope, d, fields);
            obj.define_property(scope, k, &desc);
        }
    };
    for (i, item) in items.iter().enumerate() {
        if deferred.is_empty() || deferred.get(&item_key(item)) != Some(&i) {
            define(scope, i);
        }
    }
    for key in last {
        match deferred.get(key) {
            Some(&i) => define(scope, i),
            None => {
                let k = v8_key(scope, key);
                move_to_end(scope, obj, k)?;
            }
        }
    }
    Ok(())
}

fn arg_to_v8<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    state: &SharedState,
    realm: RealmId,
    arg: &Arg,
) -> Result<v8::Local<'s, v8::Value>, HostError> {
    Ok(match arg {
        Arg::Undefined => v8::undefined(scope).into(),
        Arg::Null => v8::null(scope).into(),
        Arg::Bool(b) => v8::Boolean::new(scope, *b).into(),
        Arg::Number(n) => v8::Number::new(scope, *n).into(),
        Arg::String(s) => v8_str(scope, s).into(),
        Arg::Json(s) => {
            let s = v8_str(scope, s);
            v8::json::parse(scope, s).ok_or(HostError::Thrown)?
        }
        Arg::Handle(h) => load_handle(scope, state, *h).map_err(|e| HostError::Error(e.message))?,
        Arg::Host(iface, host) => wrap_host(scope, state, realm, *iface, *host)
            .map_err(|e| HostError::Error(e.message))?
            .into(),
        Arg::Construct(iface, args) => {
            let mat = state.borrow().mat(realm);
            let f = match &mat {
                Some(mat) => interface_object(scope, state, mat, *iface),
                None => ensure_template(scope, state, *iface).and_then(|ft| ft.get_function(scope)),
            }
            .ok_or_else(|| HostError::Error(format!("unknown interface {iface:?}")))?;
            let mut argv = Vec::with_capacity(args.len());
            for a in args {
                argv.push(arg_to_v8(scope, state, realm, a)?);
            }
            f.new_instance(scope, &argv)
                .ok_or(HostError::Thrown)?
                .into()
        }
        Arg::Array(items) => {
            let mut elems = Vec::with_capacity(items.len());
            for a in items {
                elems.push(arg_to_v8(scope, state, realm, a)?);
            }
            v8::Array::new_with_elements(scope, &elems).into()
        }
        Arg::ArrayBuffer(bytes) => new_array_buffer(scope, bytes).into(),
        Arg::Uint8Array(bytes) => {
            let len = bytes.len();
            let ab = new_array_buffer(scope, bytes);
            v8::Uint8Array::new(scope, ab, 0, len)
                .ok_or(HostError::Thrown)?
                .into()
        }
        Arg::Uint8ClampedArray(bytes) => {
            let len = bytes.len();
            let ab = new_array_buffer(scope, bytes);
            v8::Uint8ClampedArray::new(scope, ab, 0, len)
                .ok_or(HostError::Thrown)?
                .into()
        }
        Arg::Float32Array(v) => {
            let bytes: Vec<u8> = v.iter().flat_map(|x| x.to_ne_bytes()).collect();
            let ab = new_array_buffer(scope, &bytes);
            v8::Float32Array::new(scope, ab, 0, v.len())
                .ok_or(HostError::Thrown)?
                .into()
        }
        Arg::Int32Array(v) => {
            let bytes: Vec<u8> = v.iter().flat_map(|x| x.to_ne_bytes()).collect();
            let ab = new_array_buffer(scope, &bytes);
            v8::Int32Array::new(scope, ab, 0, v.len())
                .ok_or(HostError::Thrown)?
                .into()
        }
        Arg::Uint32Array(v) => {
            let bytes: Vec<u8> = v.iter().flat_map(|x| x.to_ne_bytes()).collect();
            let ab = new_array_buffer(scope, &bytes);
            v8::Uint32Array::new(scope, ab, 0, v.len())
                .ok_or(HostError::Thrown)?
                .into()
        }
    })
}

fn new_array_buffer<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    bytes: &[u8],
) -> v8::Local<'s, v8::ArrayBuffer> {
    let store = v8::ArrayBuffer::new_backing_store_from_vec(bytes.to_vec()).make_shared();
    v8::ArrayBuffer::with_backing_store(scope, &store)
}

/// Bytes of an `ArrayBuffer` or `ArrayBufferView`, copied out.
/// Element type of an `ArrayBufferView`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewKind {
    Int8,
    Uint8,
    Uint8Clamped,
    Int16,
    Uint16,
    Int32,
    Uint32,
    Float16,
    Float32,
    Float64,
    BigInt64,
    BigUint64,
    DataView,
}

fn view_kind(v: v8::Local<'_, v8::Value>) -> Option<ViewKind> {
    Some(if v.is_float32_array() {
        ViewKind::Float32
    } else if v.is_uint8_array() {
        ViewKind::Uint8
    } else if v.is_uint8_clamped_array() {
        ViewKind::Uint8Clamped
    } else if v.is_int8_array() {
        ViewKind::Int8
    } else if v.is_int16_array() {
        ViewKind::Int16
    } else if v.is_uint16_array() {
        ViewKind::Uint16
    } else if v.is_int32_array() {
        ViewKind::Int32
    } else if v.is_uint32_array() {
        ViewKind::Uint32
    } else if v.is_float64_array() {
        ViewKind::Float64
    } else if v.is_big_int64_array() {
        ViewKind::BigInt64
    } else if v.is_big_uint64_array() {
        ViewKind::BigUint64
    } else if v.is_data_view() {
        ViewKind::DataView
    } else if v.is_float16_array() {
        ViewKind::Float16
    } else {
        return None;
    })
}

fn write_view(v: v8::Local<'_, v8::Value>, offset: usize, bytes: &[u8]) -> Option<usize> {
    let view = v8::Local::<v8::ArrayBufferView>::try_from(v).ok()?;
    let len = view.byte_length();
    if offset >= len {
        return Some(0);
    }
    let n = bytes.len().min(len - offset);
    let dst = view.data() as *mut u8;
    if dst.is_null() || n == 0 {
        return Some(0);
    }
    // SAFETY: V8 has no safe API for writing into an existing typed array
    // (the out-parameter of `readPixels`, `getFloatFrequencyData`, ...).
    // `data()` points at the view's first byte (byte offset included) in
    // a live backing store; we write `n <= byte_length - offset` bytes, so
    // the write stays inside the view. A detached buffer has length 0 and
    // returns above. No JS runs while the pointer is in use.
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst.add(offset), n) };
    Some(n)
}

fn read_bytes(scope: &mut v8::PinScope<'_, '_>, v: v8::Local<'_, v8::Value>) -> Option<Vec<u8>> {
    if let Ok(view) = v8::Local::<v8::ArrayBufferView>::try_from(v) {
        let mut out = vec![0u8; view.byte_length()];
        let n = view.copy_contents(&mut out);
        out.truncate(n);
        return Some(out);
    }
    if let Ok(ab) = v8::Local::<v8::ArrayBuffer>::try_from(v) {
        let len = ab.byte_length();
        let view = v8::Uint8Array::new(scope, ab, 0, len)?;
        let mut out = vec![0u8; len];
        let n = view.copy_contents(&mut out);
        out.truncate(n);
        return Some(out);
    }
    None
}

/// Like `caught`, but leaves a termination in place so it keeps unwinding
/// the outer JS (used for calls made from inside a host callback).
fn caught_nested(tc: &mut v8::PinnedRef<'_, v8::TryCatch<'_, '_, v8::HandleScope<'_>>>) -> JsError {
    if tc.has_terminated() || tc.is_execution_terminating() {
        return JsError::new(JsErrorKind::Terminated, "Execution terminated");
    }
    match tc.exception() {
        Some(exc) => {
            let message = tc.message();
            exception_to_error(tc, exc, message)
        }
        None => JsError::new(JsErrorKind::Exception, "unknown error"),
    }
}

fn throw(scope: &mut v8::PinScope<'_, '_>, state: &SharedState, realm: RealmId, err: HostError) {
    let exception = match err {
        HostError::Thrown => return,
        HostError::TypeError(m) => {
            let m = v8_str(scope, &m);
            v8::Exception::type_error(scope, m)
        }
        HostError::RangeError(m) => {
            let m = v8_str(scope, &m);
            v8::Exception::range_error(scope, m)
        }
        HostError::SyntaxError(m) => {
            let m = v8_str(scope, &m);
            v8::Exception::syntax_error(scope, m)
        }
        HostError::ReferenceError(m) => {
            let m = v8_str(scope, &m);
            v8::Exception::reference_error(scope, m)
        }
        HostError::Error(m) => {
            let m = v8_str(scope, &m);
            v8::Exception::error(scope, m)
        }
        HostError::Throw(arg) => match arg_to_v8(scope, state, realm, &arg) {
            Ok(v) => v,
            Err(_) => return,
        },
    };
    scope.throw_exception(exception);
}

fn new_promise<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    state: &SharedState,
    realm: RealmId,
) -> Option<(v8::Local<'s, v8::Promise>, Resolver)> {
    let resolver = v8::PromiseResolver::new(scope)?;
    let promise = resolver.get_promise(scope);
    let global = v8::Global::new(scope, resolver);
    let (idx, generation) = state.borrow_mut().resolvers.insert((realm, global));
    Some((promise, Resolver { idx, generation }))
}

fn settle_promise(
    scope: &mut v8::PinScope<'_, '_>,
    state: &SharedState,
    realm: RealmId,
    resolver: Resolver,
    outcome: Result<Arg, HostError>,
) -> Result<(), HostError> {
    let global = state
        .borrow_mut()
        .resolvers
        .remove(resolver.idx, resolver.generation)
        .map(|(_, g)| g)
        .ok_or_else(|| HostError::Error("stale resolver".into()))?;
    let resolver = v8::Local::new(scope, &global);
    match outcome {
        Ok(arg) => {
            let v = arg_to_v8(scope, state, realm, &arg)?;
            resolver.resolve(scope, v);
        }
        Err(err) => {
            // Build the exception value without leaving it pending.
            let v = {
                v8::tc_scope!(let tc, scope);
                throw(tc, state, realm, err);
                tc.exception()
            };
            let v = v.unwrap_or_else(|| v8::undefined(scope).into());
            resolver.reject(scope, v);
        }
    }
    Ok(())
}

fn call_inner<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    state: &SharedState,
    realm: RealmId,
    func: Handle,
    this: &Arg,
    args: &[Arg],
) -> Result<Option<v8::Local<'s, v8::Value>>, JsError> {
    let f = load_handle(scope, state, func)?;
    let f = v8::Local::<v8::Function>::try_from(f)
        .map_err(|_| JsError::new(JsErrorKind::Host, "handle is not a function"))?;
    let conv = |scope: &mut v8::PinScope<'s, '_>, a: &Arg| arg_to_v8(scope, state, realm, a);
    let Ok(recv) = conv(scope, this) else {
        return Ok(None);
    };
    let mut argv = Vec::with_capacity(args.len());
    for a in args {
        match conv(scope, a) {
            Ok(v) => argv.push(v),
            Err(_) => return Ok(None),
        }
    }
    Ok(f.call(scope, recv, &argv))
}

// ---------------------------------------------------------------------------
// Scripts, modules, errors
// ---------------------------------------------------------------------------

fn script_origin<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    origin: ScriptOrigin<'_>,
    is_module: bool,
) -> v8::ScriptOrigin<'s> {
    let name = v8_str(scope, origin.url);
    v8::ScriptOrigin::new(
        scope,
        name.into(),
        origin.line as i32,
        origin.column as i32,
        false,
        -1,
        None,
        false,
        false,
        is_module,
        None,
    )
}

fn run_script<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    source: &str,
    origin: ScriptOrigin<'_>,
) -> Option<v8::Local<'s, v8::Value>> {
    let code = v8::String::new(scope, source)?;
    let url = origin.url;
    let origin = script_origin(scope, origin, false);
    if source.len() < CODE_CACHE_MIN {
        let script = v8::Script::compile(scope, code, Some(&origin))?;
        return script.run(scope);
    }
    // Large script: compile from this engine's code cache if it has one.
    // A snapshot creator may not make code caches (V8 aborts).
    let state = scope
        .get_slot::<SharedState>()
        .filter(|st| {
            st.try_borrow()
                .is_ok_and(|st| !matches!(st.snap, SnapState::Creating(_)))
        })
        .cloned();
    if state.is_none() {
        let script = v8::Script::compile(scope, code, Some(&origin))?;
        return script.run(scope);
    }
    let key = snapshot::hash_str(source) ^ snapshot::hash_str(url).rotate_left(1);
    // None: not seen; Some(None): compiled once before; Some(Some(_)): cached.
    let seen: Option<Option<Rc<[u8]>>> = state.as_ref().and_then(|st| {
        let st = st.try_borrow().ok()?;
        st.code_cache
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, d)| d.clone())
    });
    let cached = seen.clone().flatten();
    let script = match &cached {
        Some(bytes) => {
            let data = v8::script_compiler::CachedData::new(bytes);
            let mut src =
                v8::script_compiler::Source::new_with_cached_data(code, Some(&origin), data);
            let script = v8::script_compiler::compile(
                scope,
                &mut src,
                v8::script_compiler::CompileOptions::ConsumeCodeCache,
                v8::script_compiler::NoCacheReason::NoReason,
            )?;
            if src.get_cached_data().is_some_and(|d| d.rejected())
                && let Some(mut st) = state.as_ref().and_then(|s| s.try_borrow_mut().ok())
            {
                st.code_cache.retain(|(k, _)| *k != key);
            }
            script
        }
        None => {
            let script = v8::Script::compile(scope, code, Some(&origin))?;
            // A script seen once is only remembered; the second compile
            // makes the cache. Before the run: only what V8 compiled
            // eagerly goes in, so a consumer compiles the rest lazily as
            // usual.
            let data = match seen {
                Some(None) => script
                    .get_unbound_script(scope)
                    .create_code_cache()
                    .map(|d| Rc::<[u8]>::from(&d[..])),
                _ => None,
            };
            if let Some(mut st) = state.as_ref().and_then(|s| s.try_borrow_mut().ok()) {
                st.code_cache.retain(|(k, _)| *k != key);
                if st.code_cache.len() >= CODE_CACHE_ENTRIES {
                    st.code_cache.remove(0);
                }
                st.code_cache.push((key, data));
            }
            script
        }
    };
    script.run(scope)
}

fn compile_module<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    state: &SharedState,
    realm: RealmId,
    source: &str,
    origin: ScriptOrigin<'_>,
) -> Option<v8::Local<'s, v8::Module>> {
    let code = v8::String::new(scope, source)?;
    let v8_origin = script_origin(scope, origin, true);
    let mut src = v8::script_compiler::Source::new(code, Some(&v8_origin));
    let module = v8::script_compiler::compile_module(scope, &mut src)?;
    let global = v8::Global::new(scope, module);
    if let Some(r) = state.borrow_mut().realms.get_mut(&realm) {
        r.modules.push(ModuleEntry {
            url: origin.url.to_owned(),
            hash: module.get_identity_hash(),
            module: global,
        });
    }
    Some(module)
}

fn resolve_module<'s>(
    context: v8::Local<'s, v8::Context>,
    specifier: v8::Local<'s, v8::String>,
    _import_attributes: v8::Local<'s, v8::FixedArray>,
    referrer: v8::Local<'s, v8::Module>,
) -> Option<v8::Local<'s, v8::Module>> {
    // SAFETY: V8 calls this synchronously from `instantiate_module` on the
    // isolate's thread; the context belongs to that isolate.
    v8::callback_scope!(unsafe scope, context);
    let state = shared_state(scope);
    let realm = current_realm(scope);
    let specifier = specifier.to_rust_string_lossy(scope);

    let (loader, referrer_url) = {
        let st = state.borrow();
        let r = st.realms.get(&realm)?;
        let hash = referrer.get_identity_hash();
        let referrer_url = r
            .modules
            .iter()
            .find(|m| m.hash == hash && v8::Local::new(scope, &m.module) == referrer)
            .map(|m| m.url.clone())
            .unwrap_or_default();
        (r.loader.clone(), referrer_url)
    };
    let Some(loader) = loader else {
        let msg = format!("Failed to resolve module specifier \"{specifier}\"");
        throw(scope, &state, realm, HostError::TypeError(msg));
        return None;
    };
    let loaded = match loader(&specifier, &referrer_url) {
        Ok(l) => l,
        Err(msg) => {
            throw(scope, &state, realm, HostError::TypeError(msg));
            return None;
        }
    };
    let cached = {
        let st = state.borrow();
        st.realms
            .get(&realm)?
            .modules
            .iter()
            .find(|m| m.url == loaded.url)
            .map(|m| v8::Local::new(scope, &m.module))
    };
    if let Some(m) = cached {
        return Some(m);
    }
    compile_module(
        scope,
        &state,
        realm,
        &loaded.source,
        ScriptOrigin::new(&loaded.url),
    )
}

/// Turn the pending exception of `tc` into a [`JsError`].
fn caught(
    tc: &mut v8::PinnedRef<'_, v8::TryCatch<'_, '_, v8::HandleScope<'_>>>,
    state: &SharedState,
) -> JsError {
    if tc.has_terminated() || tc.is_execution_terminating() {
        tc.cancel_terminate_execution();
        return if state.borrow().take_oom() {
            JsError::new(JsErrorKind::OutOfMemory, "JS heap limit reached")
        } else {
            JsError::new(JsErrorKind::Terminated, "Execution terminated")
        };
    }
    match tc.exception() {
        Some(exc) => {
            let message = tc.message();
            exception_to_error(tc, exc, message)
        }
        None => JsError::new(JsErrorKind::Exception, "unknown error"),
    }
}

fn exception_to_error(
    scope: &mut v8::PinScope<'_, '_>,
    exc: v8::Local<'_, v8::Value>,
    message: Option<v8::Local<'_, v8::Message>>,
) -> JsError {
    let mut err = JsError::new(JsErrorKind::Exception, String::new());
    // Reading `message`/`stack` may run user getters; never let that throw
    // out of here.
    {
        v8::tc_scope!(let tc, scope);
        let as_object = v8::Local::<v8::Object>::try_from(exc).ok();
        let read = |tc: &mut v8::PinScope<'_, '_>, key: &str| -> Option<String> {
            let obj = as_object?;
            let k = v8_str(tc, key);
            let v = obj.get(tc, k.into())?;
            v.is_string().then(|| v.to_rust_string_lossy(tc))
        };
        if as_object.is_some() {
            err.stack = read(tc, "stack");
            if let Some(m) = read(tc, "message") {
                err.message = m;
            } else if exc.is_native_error() {
                err.message = String::new();
            }
        }
    }
    if err.message.is_empty() && !exc.is_native_error() {
        err.message = detail_string(scope, exc);
    }
    let message = message.or_else(|| Some(v8::Exception::create_message(scope, exc)));
    if let Some(msg) = message {
        if let Some(name) = msg.get_script_resource_name(scope)
            && name.is_string()
        {
            err.url = Some(name.to_rust_string_lossy(scope));
        }
        err.line = msg.get_line_number(scope).map(|l| l as u32);
        err.column = Some(msg.get_start_column() as u32);
    }
    err
}

//! JavaScript engine boundary.
//!
//! Everything above this crate (generated WebIDL bindings, Web APIs, the
//! event loop, CDP) talks to the engine through this crate. Two backends:
//! - `chrome` feature: V8 via the `v8` crate (rusty_v8). Implemented.
//! - `firefox` feature: SpiderMonkey via Servo's `mozjs` (M4b). Only the
//!   engine-neutral types and traits below exist in that build for now.
//!
//! A binary links exactly one backend, so the engine always matches the
//! browser family the persona claims.
//!
//! # Model
//!
//! - **Engine** ([`Engine`], chrome): one V8 isolate. Create one per worker
//!   thread (and one per dedicated worker). It is `!Send`: it never leaves the
//!   thread that made it. The V8 platform is initialised once per process on
//!   first use ([`init_v8`]), after installing ICU's data, which ships
//!   as zstd chunks instead of linked raw and is paged in by chunk on
//!   first touch (`icu_data`, ADR 0013).
//!   Several engines may live on one thread; every call enters its own
//!   isolate.
//! - **Realm** ([`RealmId`]): one V8 context. One per frame, plus isolated
//!   worlds for CDP utility scripts ([`World::Isolated`]). Realms of one
//!   engine share the isolate, the microtask queue and the host objects, but
//!   no JS objects: each realm has its own global, its own interface objects
//!   and its own wrapper cache.
//! - **Interfaces** ([`InterfaceSpec`], chrome): declarative description of a
//!   WebIDL interface (interface object, prototype, instance members,
//!   constants, named/indexed getters). Defined once per engine as V8
//!   templates and instantiated lazily in each realm, so property order and
//!   descriptors come out exactly as listed. The global of a realm is built
//!   from one interface (normally `Window`); for a `[Global]` interface its
//!   `instance` members are the global's own properties, including the
//!   interface objects ([`Member::interface`]).
//! - **Host objects** ([`HostRef`]): a `(kind, id)` pair the host chooses,
//!   e.g. a DOM node id. [`Engine::wrap`] / [`CallCtx::ret`] with
//!   [`Arg::Host`] create a JS wrapper with the pair in internal fields. A
//!   per-realm wrapper cache returns the same JS object for the same
//!   `HostRef` every time.
//! - **Callbacks** ([`HostFn`]): Rust closures that receive a [`CallCtx`]
//!   (arguments with WebIDL-style conversions, `this` host ref, return
//!   value). They throw by returning [`HostError`].
//! - **Installer support** (chrome): [`Member::alias`] (same function object
//!   under two keys, e.g. `@@iterator` = `entries`), [`Member::namespace`]
//!   (plain objects such as `CSS`), [`Arg::Construct`] (`new X(...)`, e.g. a
//!   `DOMException` to throw), [`CallCtx::set_this_private`] /
//!   [`CallCtx::this_private`] (per-object host state invisible to JS),
//!   [`Engine::next_interface_id`] (the global's own interface object).
//!   Instance member lists longer than one V8 template can hold (~1,000
//!   with accessors) are split; the tail is defined on each new instance.
//! - **Handles** ([`Handle`]): persistent references to JS values held by the
//!   host (event listeners, timer callbacks, promises). Released explicitly
//!   or when their realm is dropped. Stale handles are detected, never UB.
//!
//! # Example (chrome)
//!
//! ```ignore
//! let mut engine = Engine::new(Arc::new(Persona::default_chrome()), EngineOptions::default())?;
//! let node = engine.define_interface(
//!     InterfaceSpec::new("Node").prototype(vec![Member::getter("nodeType", host_fn(|cx| {
//!         let host = cx.this_host().ok_or_else(|| HostError::type_error("Illegal invocation"))?;
//!         cx.ret(host.kind)
//!     }))]),
//! );
//! let window = engine.define_interface(
//!     InterfaceSpec::new("Window").instance(vec![Member::interface("Node", node)]),
//! );
//! let realm = engine.new_realm(RealmOptions::with_global(window))?;
//! let v = engine.evaluate(realm, "typeof Node", ScriptOrigin::new("https://a.test/"))?;
//! ```
//!
//! # Startup snapshot (chrome, ADR 0013)
//!
//! [`Engine::snapshot_creator`] records interface definitions;
//! [`Engine::create_snapshot`] serializes their templates plus one context
//! of a global interface with an init script run. [`Engine::with_snapshot`]
//! restores: the host defines the same interfaces again (checked against a
//! digest each, only the callbacks are registered), then
//! [`Engine::use_snapshot_context`] lets realms of that global be
//! deserialized ([`RealmOps::is_snapshot_realm`]). Mismatches fall back to
//! building; [`Engine::snapshot_status`] tells which happened.
//!
//! # Script execution
//!
//! [`Engine::evaluate`] compiles and runs a classic script with a
//! [`ScriptOrigin`] (URL plus line/column offset, so `error.stack` shows the
//! document position). [`Engine::evaluate_module`] runs an ES module graph;
//! imports go through the realm's [`ModuleLoader`]. The microtask policy is
//! explicit: promise jobs run only in
//! [`Engine::perform_microtask_checkpoint`].
//!
//! Exceptions come back as [`JsError`] with V8's own `message` and `stack`
//! text. A per-isolate heap cap ([`EngineOptions::max_heap_bytes`]) is
//! enforced with a near-heap-limit callback that terminates the running
//! script ([`JsErrorKind::OutOfMemory`]) instead of crashing the process.

use std::rc::Rc;

use lb_persona::BrowserFamily;

#[cfg(all(feature = "chrome", feature = "firefox"))]
compile_error!("enable only one of the `chrome` / `firefox` features per binary");

#[cfg(feature = "chrome")]
mod v8_engine;
#[cfg(feature = "chrome")]
pub use v8_engine::*;

/// Family compiled into this binary, if any.
pub const COMPILED_FAMILY: Option<BrowserFamily> = if cfg!(feature = "chrome") {
    Some(BrowserFamily::Chrome)
} else if cfg!(feature = "firefox") {
    Some(BrowserFamily::Firefox)
} else {
    None
};

/// A JS value copied out of the engine.
#[derive(Debug, Clone, PartialEq)]
pub enum JsValue {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    /// An object, serialised with `JSON.stringify`.
    Json(String),
    /// A value without a JSON form (function, symbol, bigint, cyclic
    /// object): the engine's description of it, e.g. `Symbol(a)`.
    Opaque(String),
}

/// What kind of failure a [`JsError`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsErrorKind {
    /// JS threw (including syntax errors at compile time).
    Exception,
    /// Execution was terminated (watchdog, [`Engine::terminate_handle`]).
    Terminated,
    /// The heap cap was reached; the script was terminated.
    OutOfMemory,
    /// Host-side misuse: unknown realm, stale handle, bad interface id.
    Host,
}

/// An error from the engine. For exceptions, `message` and `stack` are the
/// engine's own text (`error.message` / `error.stack`), so they match the
/// real browser byte for byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsError {
    pub kind: JsErrorKind,
    /// `error.message` for Error objects; the string form of any other
    /// thrown value.
    pub message: String,
    /// `error.stack` when the thrown value has one.
    pub stack: Option<String>,
    /// Script URL of the throw site, if known.
    pub url: Option<String>,
    /// 1-based line of the throw site, if known.
    pub line: Option<u32>,
    /// 0-based column of the throw site, if known.
    pub column: Option<u32>,
}

impl JsError {
    /// An error with no source position.
    pub fn new(kind: JsErrorKind, message: impl Into<String>) -> Self {
        JsError {
            kind,
            message: message.into(),
            stack: None,
            url: None,
            line: None,
            column: None,
        }
    }

    #[cfg(feature = "chrome")]
    pub(crate) fn host(message: impl Into<String>) -> Self {
        Self::new(JsErrorKind::Host, message)
    }
}

impl std::fmt::Display for JsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.stack {
            Some(stack) => f.write_str(stack),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for JsError {}

/// Why an engine could not be created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    /// The persona claims a different family than the compiled engine.
    FamilyMismatch {
        persona: BrowserFamily,
        compiled: Option<BrowserFamily>,
    },
    /// A process can make one startup snapshot: V8 shares one read-only
    /// heap between all isolates, and a second snapshot creator would
    /// change it under engines restored from the first blob.
    SnapshotCreatorUsed,
    /// A snapshot creator must be the only engine of the process.
    SnapshotNotAlone,
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EngineError::FamilyMismatch { persona, compiled } => write!(
                f,
                "persona family {} does not match the compiled engine ({})",
                persona.as_str(),
                compiled.map_or("none", |c| c.as_str())
            ),
            EngineError::SnapshotCreatorUsed => {
                f.write_str("this process already made a startup snapshot")
            }
            EngineError::SnapshotNotAlone => {
                f.write_str("a startup snapshot can only be made while no other engine exists")
            }
        }
    }
}

impl std::error::Error for EngineError {}

/// Where a script comes from. Shown in stack traces and error positions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScriptOrigin<'a> {
    pub url: &'a str,
    /// 0-based line of the first script line in the resource (an inline
    /// `<script>` on document line 10 has offset 9).
    pub line: u32,
    /// 0-based column of the first script character on that line.
    pub column: u32,
}

impl<'a> ScriptOrigin<'a> {
    pub fn new(url: &'a str) -> Self {
        ScriptOrigin {
            url,
            line: 0,
            column: 0,
        }
    }

    /// Same origin, starting at `line`/`column` (both 0-based).
    pub fn at(self, line: u32, column: u32) -> Self {
        ScriptOrigin {
            line,
            column,
            ..self
        }
    }
}

/// A realm (frame or isolated world) inside one engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RealmId(pub u32);

/// An interface defined on one engine. Valid only on that engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InterfaceId(pub u32);

/// A host object as the host sees it, e.g. a DOM node (`kind` = node type
/// tag, `id` = arena id). Stored in the JS wrapper's internal fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HostRef {
    pub kind: u32,
    pub id: u32,
}

impl HostRef {
    pub const fn new(kind: u32, id: u32) -> Self {
        HostRef { kind, id }
    }
}

/// Which world a realm belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum World {
    /// The page's own world. Nothing automation-related goes here.
    Main,
    /// An isolated world (CDP `Page.createIsolatedWorld`), named.
    Isolated(String),
}

/// Property attributes, as in a property descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PropAttrs {
    /// Ignored for accessor properties.
    pub writable: bool,
    pub enumerable: bool,
    pub configurable: bool,
}

impl PropAttrs {
    /// WebIDL operations and attributes: writable, enumerable, configurable.
    pub const DEFAULT: PropAttrs = PropAttrs {
        writable: true,
        enumerable: true,
        configurable: true,
    };
    /// WebIDL constants: read-only, enumerable, non-configurable.
    pub const CONSTANT: PropAttrs = PropAttrs {
        writable: false,
        enumerable: true,
        configurable: false,
    };
    /// Interface objects on the global: writable, not enumerable,
    /// configurable.
    pub const HIDDEN: PropAttrs = PropAttrs {
        writable: true,
        enumerable: false,
        configurable: true,
    };
    /// `[LegacyUnforgeable]` members: not configurable.
    pub const UNFORGEABLE: PropAttrs = PropAttrs {
        writable: false,
        enumerable: true,
        configurable: false,
    };
}

impl Default for PropAttrs {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The type of a JS value, without converting it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    Undefined,
    Null,
    Bool,
    Number,
    BigInt,
    String,
    Symbol,
    Function,
    Object,
}

/// Source of one ES module, returned by a [`ModuleLoader`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleSource {
    /// Absolute URL after resolution; the module map key.
    pub url: String,
    pub source: String,
}

/// Resolves and loads an import: `(specifier, referrer_url)` to the module
/// source. Errors become a `TypeError` in JS with the given message.
pub type ModuleLoader = Rc<dyn Fn(&str, &str) -> Result<ModuleSource, String>>;

/// How to build a new realm.
#[derive(Clone)]
pub struct RealmOptions {
    /// Interface whose instance template becomes the global (normally
    /// `Window`). `None` gives a plain JS global.
    pub global: Option<InterfaceId>,
    /// Host object behind the global (the window); the global becomes its
    /// wrapper in this realm.
    pub global_host: Option<HostRef>,
    pub world: World,
    /// Import resolution for ES modules. Without it, imports fail.
    pub module_loader: Option<ModuleLoader>,
}

impl RealmOptions {
    /// Main-world realm whose global is built from `global`.
    pub fn with_global(global: InterfaceId) -> Self {
        RealmOptions {
            global: Some(global),
            ..Self::default()
        }
    }
}

impl Default for RealmOptions {
    fn default() -> Self {
        RealmOptions {
            global: None,
            global_host: None,
            world: World::Main,
            module_loader: None,
        }
    }
}

impl std::fmt::Debug for RealmOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RealmOptions")
            .field("global", &self.global)
            .field("global_host", &self.global_host)
            .field("world", &self.world)
            .field("module_loader", &self.module_loader.is_some())
            .finish()
    }
}

/// Heap numbers for one engine, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HeapStats {
    pub used: usize,
    pub total: usize,
    pub limit: usize,
    /// Times the near-heap-limit callback fired.
    pub limit_hits: u32,
}

/// One realm of an engine, borrowed from it.
pub trait Realm {
    fn id(&self) -> RealmId;
    /// Compile and run a classic script.
    fn evaluate(&mut self, source: &str, origin: ScriptOrigin<'_>) -> Result<JsValue, JsError>;
    /// Run queued promise jobs until empty. The queue is per engine, so this
    /// also runs jobs of the engine's other realms.
    fn perform_microtask_checkpoint(&mut self);
}

/// An engine instance (V8 isolate / SpiderMonkey runtime). Single-threaded:
/// pinned to the worker thread that created it.
pub trait JsEngine {
    type Realm<'e>: Realm
    where
        Self: 'e;

    fn family(&self) -> BrowserFamily;
    fn new_realm(&mut self, options: RealmOptions) -> Result<RealmId, JsError>;
    /// Borrow a realm; `None` if it was dropped or never existed.
    fn realm(&mut self, id: RealmId) -> Option<Self::Realm<'_>>;
    /// Drop a realm with its wrappers and handles in one step.
    fn drop_realm(&mut self, id: RealmId);
    fn perform_microtask_checkpoint(&mut self);
    /// Hint from the event loop when the page is idle.
    fn low_memory_notification(&mut self);
    fn heap_stats(&mut self) -> HeapStats;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiled_family_matches_feature() {
        #[cfg(feature = "chrome")]
        assert_eq!(COMPILED_FAMILY, Some(BrowserFamily::Chrome));
        #[cfg(feature = "firefox")]
        assert_eq!(COMPILED_FAMILY, Some(BrowserFamily::Firefox));
    }

    #[test]
    fn origin_offsets() {
        let o = ScriptOrigin::new("https://a.test/x.js").at(9, 4);
        assert_eq!((o.url, o.line, o.column), ("https://a.test/x.js", 9, 4));
    }
}

//! Implementations of WebIDL members, keyed by interface and member name.
//!
//! The installer looks up every member of the generated tables here. Found:
//! the `HostFn` backs the property. Not found: a stub (see
//! [`crate::install`]). Register everything before
//! [`crate::Bindings::define`]: the functions are baked into the engine's
//! templates there.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::rc::Rc;

use lb_js::{
    HostFn, IndexedGetter, IndexedLength, NamedDeleter, NamedEnumerator, NamedGetter, NamedSetter,
};

use crate::dom_exception::DomExceptions;

/// What a registered function implements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImplKind {
    /// Attribute getter (prototype, instance, global or namespace).
    Getter,
    /// Attribute setter.
    Setter,
    /// Operation (prototype, instance, global or namespace).
    Method,
    /// `new X(...)`; the member name is ignored (use `""`).
    Constructor,
    /// Static attribute getter on the interface object.
    StaticGetter,
    /// Static attribute setter.
    StaticSetter,
    /// Static operation on the interface object.
    StaticMethod,
    /// `[LegacyFactoryFunction]` (`new Image()`); member is the factory
    /// name (`"Image"`).
    LegacyFactory,
}

/// Hasher for the registry's keys: the multiply-rotate hash of rustc's
/// `FxHasher`. The keys are the binary's own interface and member names
/// (no outside input), so SipHash's flooding resistance buys nothing, and
/// it was a fifth of a `fetch` process's binding setup (~4,000 inserts and
/// lookups per engine).
#[derive(Default, Clone, Copy)]
pub struct KeyHasher(u64);

impl Hasher for KeyHasher {
    fn write(&mut self, bytes: &[u8]) {
        let mut chunks = bytes.chunks_exact(8);
        for c in &mut chunks {
            self.add(u64::from_le_bytes(c.try_into().expect("8 bytes")));
        }
        for &b in chunks.remainder() {
            self.add(u64::from(b));
        }
    }

    fn write_u8(&mut self, i: u8) {
        self.add(u64::from(i));
    }

    fn write_usize(&mut self, i: usize) {
        self.add(i as u64);
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

impl KeyHasher {
    #[inline]
    fn add(&mut self, w: u64) {
        self.0 = (self.0.rotate_left(5) ^ w).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
}

type KeyMap<K, V> = HashMap<K, V, BuildHasherDefault<KeyHasher>>;

/// Registered implementations. Cheap to build; one per engine.
#[derive(Clone, Default)]
pub struct Registry {
    fns: KeyMap<(&'static str, &'static str, ImplKind), HostFn>,
    named: KeyMap<&'static str, NamedGetter>,
    indexed: KeyMap<&'static str, IndexedGetter>,
    named_set: KeyMap<&'static str, NamedSetter>,
    named_delete: KeyMap<&'static str, NamedDeleter>,
    named_enum: KeyMap<&'static str, NamedEnumerator>,
    indexed_len: KeyMap<&'static str, IndexedLength>,
    unenumerable: std::collections::HashSet<&'static str, BuildHasherDefault<KeyHasher>>,
    dom_exceptions: DomExceptions,
    inherit: Option<Rc<Inherit>>,
}

/// Inheritance of redeclared members (see [`Registry::inherit_redeclared`]).
struct Inherit {
    table: &'static crate::desc::Table,
    keep: &'static [(&'static str, &'static str)],
    parents: std::cell::OnceCell<KeyMap<&'static str, &'static str>>,
}

impl Inherit {
    fn parent_of(&self, iface: &str) -> Option<&'static str> {
        self.parents
            .get_or_init(|| {
                self.table
                    .interfaces
                    .iter()
                    .filter_map(|d| Some((d.name, d.parent?)))
                    .collect()
            })
            .get(iface)
            .copied()
    }
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `f` for `iface.member`. A later registration replaces an
    /// earlier one.
    pub fn register(
        &mut self,
        iface: &'static str,
        member: &'static str,
        kind: ImplKind,
        f: HostFn,
    ) -> &mut Self {
        self.fns.insert((iface, member, kind), f);
        self
    }

    /// Shorthand for [`ImplKind::Getter`].
    pub fn getter(&mut self, iface: &'static str, member: &'static str, f: HostFn) -> &mut Self {
        self.register(iface, member, ImplKind::Getter, f)
    }

    /// Shorthand for [`ImplKind::Setter`].
    pub fn setter(&mut self, iface: &'static str, member: &'static str, f: HostFn) -> &mut Self {
        self.register(iface, member, ImplKind::Setter, f)
    }

    /// Shorthand for [`ImplKind::Method`].
    pub fn method(&mut self, iface: &'static str, member: &'static str, f: HostFn) -> &mut Self {
        self.register(iface, member, ImplKind::Method, f)
    }

    /// Shorthand for [`ImplKind::Constructor`].
    pub fn constructor(&mut self, iface: &'static str, f: HostFn) -> &mut Self {
        self.register(iface, "", ImplKind::Constructor, f)
    }

    /// Named property getter for instances of `iface` (`storage.foo`).
    pub fn named_getter(&mut self, iface: &'static str, f: NamedGetter) -> &mut Self {
        self.named.insert(iface, f);
        self
    }

    /// Indexed property getter for instances of `iface` (`list[0]`).
    pub fn indexed_getter(&mut self, iface: &'static str, f: IndexedGetter) -> &mut Self {
        self.indexed.insert(iface, f);
        self
    }

    /// Named property setter (`storage.foo = v`); needs a named getter.
    pub fn named_setter(&mut self, iface: &'static str, f: NamedSetter) -> &mut Self {
        self.named_set.insert(iface, f);
        self
    }

    /// Named property deleter (`delete storage.foo`); needs a named getter.
    pub fn named_deleter(&mut self, iface: &'static str, f: NamedDeleter) -> &mut Self {
        self.named_delete.insert(iface, f);
        self
    }

    /// Supported property names (`Object.keys(storage)`); needs a named
    /// getter.
    pub fn named_enumerator(&mut self, iface: &'static str, f: NamedEnumerator) -> &mut Self {
        self.named_enum.insert(iface, f);
        self
    }

    /// Number of supported indices, so indexed properties enumerate with
    /// Blink's read-only descriptor (see [`lb_js::IndexedLength`]); needs
    /// an indexed getter.
    pub fn indexed_length(&mut self, iface: &'static str, f: IndexedLength) -> &mut Self {
        self.indexed_len.insert(iface, f);
        self
    }

    /// `[LegacyUnenumerableNamedProperties]` for `iface`; needs a named
    /// getter.
    pub fn named_unenumerable(&mut self, iface: &'static str) -> &mut Self {
        self.unenumerable.insert(iface);
        self
    }

    pub fn indexed_length_for(&self, iface: &'static str) -> Option<&IndexedLength> {
        self.indexed_len.get(iface)
    }

    pub fn is_named_unenumerable(&self, iface: &'static str) -> bool {
        self.unenumerable.contains(iface)
    }

    pub fn named_setter_for(&self, iface: &'static str) -> Option<&NamedSetter> {
        self.named_set.get(iface)
    }

    pub fn named_deleter_for(&self, iface: &'static str) -> Option<&NamedDeleter> {
        self.named_delete.get(iface)
    }

    pub fn named_enumerator_for(&self, iface: &'static str) -> Option<&NamedEnumerator> {
        self.named_enum.get(iface)
    }

    pub fn get(
        &self,
        iface: &'static str,
        member: &'static str,
        kind: ImplKind,
    ) -> Option<&HostFn> {
        self.fns.get(&(iface, member, kind))
    }

    /// An attribute or operation `iface` itself declares: [`Registry::get`],
    /// or with [`Registry::inherit_redeclared`] on, the implementation of
    /// the nearest ancestor that has one.
    pub fn get_declared(
        &self,
        iface: &'static str,
        member: &'static str,
        kind: ImplKind,
    ) -> Option<&HostFn> {
        if let Some(f) = self.fns.get(&(iface, member, kind)) {
            return Some(f);
        }
        let inh = self.inherit.as_ref()?;
        if !matches!(kind, ImplKind::Getter | ImplKind::Setter | ImplKind::Method)
            || inh.keep.contains(&(iface, member))
        {
            return None;
        }
        let mut parent = inh.parent_of(iface);
        while let Some(p) = parent {
            if let Some(f) = self.fns.get(&(p, member, kind)) {
                return Some(f);
            }
            parent = inh.parent_of(p);
        }
        None
    }

    /// A member an interface of `table` redeclares (WebIDL repeats
    /// `remove()` on `CharacterData` and `Element` from `ChildNode`)
    /// without its own implementation uses its nearest ancestor's, except
    /// the `(interface, member)` pairs in `keep` (a different meaning).
    /// Looked up per member as specs are made ([`Registry::get_declared`]).
    pub fn inherit_redeclared(
        &mut self,
        table: &'static crate::desc::Table,
        keep: &'static [(&'static str, &'static str)],
    ) {
        self.inherit = Some(Rc::new(Inherit {
            table,
            keep,
            parents: std::cell::OnceCell::new(),
        }));
    }

    pub fn named_getter_for(&self, iface: &'static str) -> Option<&NamedGetter> {
        self.named.get(iface)
    }

    pub fn indexed_getter_for(&self, iface: &'static str) -> Option<&IndexedGetter> {
        self.indexed.get(iface)
    }

    /// Number of registered functions (getters, setters, methods, ...).
    pub fn len(&self) -> usize {
        self.fns.len() + self.named.len() + self.indexed.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Factory for throwable `DOMException`s. Clone it into host functions
    /// before [`crate::Bindings::define`]; it works once bindings exist.
    pub fn dom_exceptions(&self) -> DomExceptions {
        self.dom_exceptions.clone()
    }
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut keys: Vec<_> = self.fns.keys().collect();
        keys.sort_by_key(|(i, m, _)| (*i, *m));
        f.debug_struct("Registry")
            .field("fns", &keys)
            .field("named", &self.named.keys().collect::<Vec<_>>())
            .field("indexed", &self.indexed.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// Wrap a closure as a [`HostFn`] (re-export of `lb_js::host_fn` for
/// callers that only depend on this crate).
pub fn host_fn(
    f: impl Fn(&mut lb_js::CallCtx<'_, '_, '_>) -> Result<(), lb_js::HostError> + 'static,
) -> HostFn {
    Rc::new(f)
}

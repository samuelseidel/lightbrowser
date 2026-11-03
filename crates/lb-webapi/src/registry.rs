//! Implementations of WebIDL members, keyed by interface and member name.
//!
//! The installer looks up every member of the generated tables here. Found:
//! the `HostFn` backs the property. Not found: a stub (see
//! [`crate::install`]). Register everything before
//! [`crate::Bindings::define`]: the functions are baked into the engine's
//! templates there.

use std::collections::HashMap;
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

/// Registered implementations. Cheap to build; one per engine.
#[derive(Clone, Default)]
pub struct Registry {
    fns: HashMap<(&'static str, &'static str, ImplKind), HostFn>,
    named: HashMap<&'static str, NamedGetter>,
    indexed: HashMap<&'static str, IndexedGetter>,
    named_set: HashMap<&'static str, NamedSetter>,
    named_delete: HashMap<&'static str, NamedDeleter>,
    named_enum: HashMap<&'static str, NamedEnumerator>,
    indexed_len: HashMap<&'static str, IndexedLength>,
    unenumerable: std::collections::HashSet<&'static str>,
    dom_exceptions: DomExceptions,
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

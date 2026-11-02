//! Implementations of WebIDL members, keyed by interface and member name.
//!
//! The installer looks up every member of the generated tables here. Found:
//! the `HostFn` backs the property. Not found: a stub (see
//! [`crate::install`]). Register everything before
//! [`crate::Bindings::define`]: the functions are baked into the engine's
//! templates there.

use std::collections::HashMap;
use std::rc::Rc;

use lb_js::{HostFn, IndexedGetter, NamedGetter};

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

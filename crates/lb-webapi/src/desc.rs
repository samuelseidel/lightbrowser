//! Descriptor types for the generated binding tables
//! (`generated/chromium.rs`). Pure data: the installer turns them into
//! `lb_js::InterfaceSpec`s. Engine-neutral, so the firefox build compiles
//! them too.
//!
//! The tables hold no pointers: strings are [`S`] (offset and length in
//! one string pool) and lists are [`L`] (a range of a per-type pool). In a
//! position-independent executable every pointer in static data is
//! relocated at load, which writes, and so copies into private memory,
//! every page holding one: as `&'static str` and slices these tables were
//! ~0.6 MB of resident memory in every process. Pointer-free, they stay
//! clean file pages that are read only where used (ADR 0016). Both deref
//! to `str` and `[T]`.

use std::marker::PhantomData;
use std::ops::Deref;

/// A string of the generated table (see the module docs).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct S {
    off: u32,
    len: u32,
}

impl S {
    pub const EMPTY: S = S { off: 0, len: 0 };

    pub const fn new(off: u32, len: u32) -> S {
        S { off, len }
    }

    /// The string, for as long as the program runs.
    pub fn get(self) -> &'static str {
        let pool = crate::generated::chromium::STRINGS;
        &pool[self.off as usize..(self.off + self.len) as usize]
    }
}

impl Deref for S {
    type Target = str;
    fn deref(&self) -> &str {
        self.get()
    }
}

impl std::fmt::Debug for S {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self.get(), f)
    }
}

impl std::fmt::Display for S {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.get())
    }
}

impl PartialEq<str> for S {
    fn eq(&self, other: &str) -> bool {
        self.get() == other
    }
}

impl From<S> for &'static str {
    fn from(s: S) -> &'static str {
        s.get()
    }
}

impl From<S> for String {
    fn from(s: S) -> String {
        s.get().to_owned()
    }
}

#[cfg(feature = "chrome")]
impl From<S> for lb_js::PropKey {
    fn from(s: S) -> lb_js::PropKey {
        lb_js::PropKey::Name(s.get().to_owned())
    }
}

impl PartialEq<&str> for S {
    fn eq(&self, other: &&str) -> bool {
        self.get() == *other
    }
}

/// Element types of a generated pool (see [`L`]).
pub trait Pooled: Sized + 'static {
    fn pool() -> &'static [Self];
}

/// A list of the generated table: a range of the pool of `T`.
pub struct L<T> {
    start: u32,
    len: u32,
    _t: PhantomData<T>,
}

impl<T> L<T> {
    pub const EMPTY: L<T> = L::new(0, 0);

    pub const fn new(start: u32, len: u32) -> L<T> {
        L {
            start,
            len,
            _t: PhantomData,
        }
    }
}

impl<T> Clone for L<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for L<T> {}

impl<T: Pooled> L<T> {
    /// The items, for as long as the program runs.
    pub fn get(self) -> &'static [T] {
        &T::pool()[self.start as usize..(self.start + self.len) as usize]
    }
}

impl<T: Pooled> Deref for L<T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        self.get()
    }
}

impl<T: Pooled> IntoIterator for L<T> {
    type Item = &'static T;
    type IntoIter = std::slice::Iter<'static, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.get().iter()
    }
}

impl<T: Pooled + std::fmt::Debug> std::fmt::Debug for L<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.get()).finish()
    }
}

impl<T: Pooled + PartialEq> PartialEq for L<T> {
    fn eq(&self, other: &Self) -> bool {
        self.get() == other.get()
    }
}

impl<T: Pooled + Eq> Eq for L<T> {}

impl Pooled for AttrDesc {
    fn pool() -> &'static [Self] {
        &crate::generated::chromium::ATTRS
    }
}

impl Pooled for OpDesc {
    fn pool() -> &'static [Self] {
        &crate::generated::chromium::OPS
    }
}

impl Pooled for Slot {
    fn pool() -> &'static [Self] {
        &crate::generated::chromium::SLOTS
    }
}

impl Pooled for S {
    fn pool() -> &'static [Self] {
        &crate::generated::chromium::NAMES
    }
}

impl Pooled for GlobalProp {
    fn pool() -> &'static [Self] {
        &crate::generated::chromium::WORKER_PROPS
    }
}

impl Pooled for IfaceOrder {
    fn pool() -> &'static [Self] {
        &crate::generated::chromium::IFACE_ORDERS
    }
}

impl Pooled for (S, ConstValue) {
    fn pool() -> &'static [Self] {
        &crate::generated::chromium::CONSTS
    }
}

/// Where an interface or member is exposed (WebIDL `[Exposed]`). A bit set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Exposed(pub u16);

impl Exposed {
    pub const NONE: Exposed = Exposed(0);
    pub const WINDOW: Exposed = Exposed(1);
    pub const DEDICATED_WORKER: Exposed = Exposed(2);
    pub const SHARED_WORKER: Exposed = Exposed(4);
    pub const SERVICE_WORKER: Exposed = Exposed(8);
    /// Any worklet (audio, paint, layout, animation, shared storage).
    pub const WORKLET: Exposed = Exposed(16);
    pub const SHADOW_REALM: Exposed = Exposed(32);
    pub const WORKER: Exposed = Exposed(2 | 4 | 8);

    pub const fn contains(self, other: Exposed) -> bool {
        self.0 & other.0 == other.0 && other.0 != 0
    }
    pub const fn intersects(self, other: Exposed) -> bool {
        self.0 & other.0 != 0
    }
}

/// Value of a WebIDL constant.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConstValue {
    Number(f64),
    Bool(bool),
}

/// Coarse attribute type, enough for a spec-shaped stub default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttrType {
    /// Interfaces, dictionaries, sequences, unions, `any`: stub returns
    /// `undefined`.
    Other,
    /// Nullable types and `EventHandler`: stub returns `null`.
    Null,
    Bool,
    Number,
    String,
}

/// One WebIDL attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttrDesc {
    pub name: S,
    /// No setter (unless `replaceable` or `put_forwards`).
    pub readonly: bool,
    pub is_static: bool,
    /// `[LegacyUnforgeable]`: non-configurable, on the instance.
    pub unforgeable: bool,
    /// `[Replaceable]`: readonly, but assignment shadows it with a data
    /// property.
    pub replaceable: bool,
    /// `[SameObject]`: the getter must return the same object every time.
    pub same_object: bool,
    /// `[PutForwards=x]`: assignment sets `this[name][x]`.
    pub put_forwards: Option<S>,
    /// Lives on the instance, not the prototype (`[LegacyUnforgeable]`, or
    /// any member of a `[Global]` interface).
    pub on_instance: bool,
    /// False for Blink's `[NotEnumerable]`.
    pub enumerable: bool,
    /// Chrome installs it after `constructor` (conditionally enabled
    /// feature), so it comes after `constructor` in property order.
    pub late: bool,
    pub ty: AttrType,
    /// Only in secure contexts (`[SecureContext]`, as captured).
    pub secure_context: bool,
}

impl AttrDesc {
    /// Base for `..AttrDesc::DEFAULT` in the generated tables.
    pub const DEFAULT: AttrDesc = AttrDesc {
        name: S::EMPTY,
        readonly: false,
        is_static: false,
        unforgeable: false,
        replaceable: false,
        same_object: false,
        put_forwards: None,
        on_instance: false,
        enumerable: true,
        late: false,
        ty: AttrType::Other,
        secure_context: false,
    };

    /// Whether the property has a setter.
    pub const fn has_setter(&self) -> bool {
        !self.readonly || self.replaceable || self.put_forwards.is_some()
    }
}

/// Built-in `Array.prototype` functions reused by indexed iterables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrayIntrinsic {
    Entries,
    Keys,
    Values,
    ForEach,
}

/// Where an operation comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    /// A named operation in the IDL.
    Regular,
    /// `toString` from a `stringifier`.
    Stringifier,
    /// Generated by `iterable<>`, `async iterable<>`, `maplike<>` or
    /// `setlike<>` (`entries`, `forEach`, `get`, `add`, ...).
    Iteration,
    /// The same function object as `Array.prototype.<x>`.
    Array(ArrayIntrinsic),
}

/// One WebIDL operation (all overloads under one name).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpDesc {
    pub name: S,
    /// `Function.length`: required arguments of the shortest overload.
    pub length: u8,
    pub is_static: bool,
    pub unforgeable: bool,
    pub on_instance: bool,
    pub enumerable: bool,
    pub late: bool,
    pub kind: OpKind,
    /// Only in secure contexts.
    pub secure_context: bool,
}

impl OpDesc {
    pub const DEFAULT: OpDesc = OpDesc {
        name: S::EMPTY,
        length: 0,
        is_static: false,
        unforgeable: false,
        on_instance: false,
        enumerable: true,
        late: false,
        kind: OpKind::Regular,
        secure_context: false,
    };
}

/// A reference into an interface's member lists, used for property order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Const(u16),
    Attr(u16),
    Op(u16),
}

/// What kind of iteration an interface declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Iterable {
    None,
    Value,
    Pair,
    AsyncValue,
    AsyncPair,
    Maplike { readonly: bool },
    Setlike { readonly: bool },
}

/// `X.prototype[Symbol.iterator]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolIterator {
    None,
    /// `Array.prototype.values` (indexed getter).
    ArrayValues,
    /// Same function object as the named prototype method (`entries` for
    /// pair iterables and maplike, `values` for setlike).
    Alias(S),
}

/// One WebIDL interface (or callback interface with constants).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InterfaceDesc {
    pub name: S,
    /// Parent interface; `None` means `Object.prototype`.
    pub parent: Option<S>,
    pub exposed: Exposed,
    /// `[Global]` (Window, worker global scopes): members live on the
    /// instance.
    pub is_global: bool,
    /// `[LegacyWindowAlias]`: extra global names for the same interface
    /// object.
    pub legacy_window_alias: L<S>,
    pub has_constructor: bool,
    /// `X.length`.
    pub constructor_length: u8,
    /// `[LegacyFactoryFunction=Name(...)]`, e.g. `Image`.
    pub legacy_factory: Option<S>,
    pub legacy_factory_length: u8,
    pub constants: L<(S, ConstValue)>,
    pub attributes: L<AttrDesc>,
    pub operations: L<OpDesc>,
    pub stringifier: bool,
    /// `stringifier attribute x`: `toString` returns this attribute.
    pub stringifier_attr: Option<S>,
    pub iterable: Iterable,
    pub indexed_getter: bool,
    pub named_getter: bool,
    pub indexed_setter: bool,
    pub named_setter: bool,
    pub named_deleter: bool,
    /// The interface itself is `[LegacyUnforgeable]` (all members on the
    /// instance).
    pub legacy_unforgeable: bool,
    /// `Symbol.toStringTag` of the prototype; empty for none.
    pub to_string_tag: S,
    /// `X.prototype` own string keys in Chrome's order (`constructor`
    /// comes after the non-late slots).
    pub proto_order: L<Slot>,
    /// `X` own keys after `length`, `name`, `prototype`.
    pub static_order: L<Slot>,
    /// Own keys of every instance (unforgeable members; for `[Global]`
    /// interfaces see [`GlobalProp`]).
    pub instance_order: L<Slot>,
    pub iterator: SymbolIterator,
    /// `Symbol.asyncIterator` alias (async iterables).
    pub async_iterator: Option<S>,
    /// Names in `X.prototype[Symbol.unscopables]`.
    pub unscopables: L<S>,
    /// A `callback interface` with constants (`NodeFilter`).
    pub callback_interface: bool,
    /// The prototype inherits from a JS built-in's prototype instead
    /// (`Error` for `DOMException`).
    pub builtin_parent: Option<S>,
    /// The interface object exists only in secure contexts.
    pub secure_context: bool,
    /// Member order and descriptors were checked against the captured
    /// Chrome surface (otherwise IDL order with features guessed).
    pub from_fixture: bool,
}

impl InterfaceDesc {
    /// Base for `..InterfaceDesc::DEFAULT` in the generated tables.
    pub const DEFAULT: InterfaceDesc = InterfaceDesc {
        name: S::EMPTY,
        parent: None,
        exposed: Exposed::WINDOW,
        is_global: false,
        legacy_window_alias: L::EMPTY,
        has_constructor: false,
        constructor_length: 0,
        legacy_factory: None,
        legacy_factory_length: 0,
        constants: L::EMPTY,
        attributes: L::EMPTY,
        operations: L::EMPTY,
        stringifier: false,
        stringifier_attr: None,
        iterable: Iterable::None,
        indexed_getter: false,
        named_getter: false,
        indexed_setter: false,
        named_setter: false,
        named_deleter: false,
        legacy_unforgeable: false,
        to_string_tag: S::EMPTY,
        proto_order: L::EMPTY,
        static_order: L::EMPTY,
        instance_order: L::EMPTY,
        iterator: SymbolIterator::None,
        async_iterator: None,
        unscopables: L::EMPTY,
        callback_interface: false,
        builtin_parent: None,
        secure_context: false,
        from_fixture: false,
    };

    pub fn slot_name(&self, s: Slot) -> &'static str {
        match s {
            Slot::Const(i) => self.constants.get()[i as usize].0.get(),
            Slot::Attr(i) => self.attributes.get()[i as usize].name.get(),
            Slot::Op(i) => self.operations.get()[i as usize].name.get(),
        }
    }
}

/// A WebIDL namespace (`CSS`), a plain object on the global.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NamespaceDesc {
    pub name: S,
    pub exposed: Exposed,
    pub constants: L<(S, ConstValue)>,
    pub attributes: L<AttrDesc>,
    pub operations: L<OpDesc>,
    pub order: L<Slot>,
    pub to_string_tag: S,
}

/// One own property of the window global, in Chrome's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlobalProp {
    /// Installed by the JS engine (`Object`, `console`, `Temporal`, ...).
    Builtin(S),
    /// An interface object.
    Interface(S),
    /// `[LegacyWindowAlias]`: `name` is the interface object of `target`.
    Alias {
        name: S,
        target: S,
    },
    /// `[LegacyFactoryFunction]` of `iface` (`Image`, `Option`, `Audio`).
    LegacyFactory {
        name: S,
        iface: S,
    },
    Namespace(S),
    /// An attribute of the global interface.
    Attr(S),
    /// An operation of the global interface.
    Op(S),
    /// Not from WebIDL (e.g. `chrome`); M4 decides.
    Other(S),
}

impl GlobalProp {
    pub fn name(&self) -> &'static str {
        match *self {
            GlobalProp::Builtin(n)
            | GlobalProp::Interface(n)
            | GlobalProp::Namespace(n)
            | GlobalProp::Attr(n)
            | GlobalProp::Op(n)
            | GlobalProp::Other(n) => n.get(),
            GlobalProp::Alias { name, .. } | GlobalProp::LegacyFactory { name, .. } => name.get(),
        }
    }
}

/// A full binding table for one browser family.
#[derive(Debug, Clone, Copy)]
pub struct Table {
    /// Every interface, parents before children.
    pub interfaces: &'static [InterfaceDesc],
    pub namespaces: &'static [NamespaceDesc],
    /// Own properties of the window global, in order.
    pub window_props: &'static [GlobalProp],
    /// Index into `window_props` from which Chrome installs properties after
    /// the engine's late built-ins (conditionally enabled features).
    pub window_late_start: usize,
    /// Names in `window_props` that exist only in secure contexts.
    pub window_secure_only: &'static [S],
    /// Members Chrome has on some OSes only (`navigator.share`, Web
    /// Bluetooth). Realm setup deletes the gates the persona turns off.
    pub os_gated: &'static [OsGated],
    /// Worker global scopes: dedicated, then shared (ADR 0021).
    pub workers: &'static [WorkerScopeDesc],
}

/// One worker global scope (`DedicatedWorkerGlobalScope`,
/// `SharedWorkerGlobalScope`), from the captured worker surface.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorkerScopeDesc {
    /// The global interface.
    pub global: S,
    /// `Exposed::DEDICATED_WORKER` or `Exposed::SHARED_WORKER`.
    pub exposed: Exposed,
    /// Own properties of the global, in Chrome's order.
    pub props: L<GlobalProp>,
    /// Index into `props` from which properties come after the engine's
    /// late built-ins.
    pub late_start: usize,
    /// Interfaces whose members or order differ from their table entry in
    /// this scope (`Performance` without `timing`, ...).
    pub overrides: L<IfaceOrder>,
}

impl WorkerScopeDesc {
    /// The order override of `iface` in this scope, if any.
    pub fn order(&self, iface: &str) -> Option<&'static IfaceOrder> {
        self.overrides.get().iter().find(|o| o.iface == iface)
    }
}

/// An interface's prototype and static members in one worker scope (slots
/// of the interface's own member lists).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IfaceOrder {
    pub iface: S,
    pub proto_order: L<Slot>,
    /// Prototype members from this index on come after `constructor`.
    pub ctor_at: usize,
    pub static_order: L<Slot>,
}

/// One OS-gated member: `owner` is an interface (prototype or static
/// member) or `Window` (a global property).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OsGated {
    /// `share` or `bluetooth` (see `lb_persona::OsApis`).
    pub gate: S,
    pub owner: S,
    pub name: S,
}

impl Table {
    pub fn interface(&self, name: &str) -> Option<&'static InterfaceDesc> {
        self.interfaces.iter().find(|i| i.name.get() == name)
    }
    pub fn namespace(&self, name: &str) -> Option<&'static NamespaceDesc> {
        self.namespaces.iter().find(|n| n.name.get() == name)
    }
    /// The worker scope whose global is `global`.
    pub fn worker(&self, global: &str) -> Option<&'static WorkerScopeDesc> {
        self.workers.iter().find(|w| w.global.get() == global)
    }
}

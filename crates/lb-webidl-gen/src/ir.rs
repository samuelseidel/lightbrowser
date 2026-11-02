//! Owned form of the binding tables. Mirrors `lb_webapi::desc`; the
//! emitter prints it as static Rust data.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttrTy {
    Other,
    Null,
    Bool,
    Number,
    String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Attr {
    pub name: String,
    pub readonly: bool,
    pub is_static: bool,
    pub unforgeable: bool,
    pub replaceable: bool,
    pub same_object: bool,
    pub put_forwards: Option<String>,
    pub on_instance: bool,
    pub enumerable: bool,
    pub late: bool,
    pub ty: AttrTy,
}

impl Attr {
    pub fn has_setter(&self) -> bool {
        !self.readonly || self.replaceable || self.put_forwards.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    Regular,
    Stringifier,
    Iteration,
    /// `Entries`, `Keys`, `Values` or `ForEach`.
    Array(&'static str),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Op {
    pub name: String,
    pub length: u8,
    pub is_static: bool,
    pub unforgeable: bool,
    pub on_instance: bool,
    pub enumerable: bool,
    pub late: bool,
    pub kind: OpKind,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConstV {
    Number(f64),
    Bool(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Const(usize),
    Attr(usize),
    Op(usize),
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymIter {
    None,
    ArrayValues,
    Alias(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Interface {
    pub name: String,
    pub parent: Option<String>,
    pub exposed: u16,
    pub is_global: bool,
    pub legacy_window_alias: Vec<String>,
    pub has_constructor: bool,
    pub constructor_length: u8,
    pub legacy_factory: Option<(String, u8)>,
    pub constants: Vec<(String, ConstV)>,
    pub attributes: Vec<Attr>,
    pub operations: Vec<Op>,
    pub stringifier: bool,
    pub stringifier_attr: Option<String>,
    pub iterable: Iterable,
    pub indexed_getter: bool,
    pub named_getter: bool,
    pub indexed_setter: bool,
    pub named_setter: bool,
    pub named_deleter: bool,
    pub legacy_unforgeable: bool,
    pub to_string_tag: String,
    pub proto_order: Vec<Slot>,
    pub static_order: Vec<Slot>,
    pub instance_order: Vec<Slot>,
    pub iterator: SymIter,
    pub async_iterator: Option<String>,
    pub unscopables: Vec<String>,
    pub callback_interface: bool,
    pub from_fixture: bool,
    /// The prototype's parent is a JS built-in (`Error` for DOMException).
    pub builtin_parent: Option<String>,
}

impl Interface {
    /// Name of a slot's member.
    pub fn slot_name(&self, s: Slot) -> &str {
        match s {
            Slot::Const(i) => &self.constants[i].0,
            Slot::Attr(i) => &self.attributes[i].name,
            Slot::Op(i) => &self.operations[i].name,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Namespace {
    pub name: String,
    pub exposed: u16,
    pub constants: Vec<(String, ConstV)>,
    pub attributes: Vec<Attr>,
    pub operations: Vec<Op>,
    pub order: Vec<Slot>,
    pub to_string_tag: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GlobalProp {
    Builtin(String),
    Interface(String),
    Alias { name: String, target: String },
    LegacyFactory { name: String, iface: String },
    Namespace(String),
    Attr(String),
    Op(String),
    Other(String),
}

impl GlobalProp {
    pub fn name(&self) -> &str {
        match self {
            GlobalProp::Builtin(n)
            | GlobalProp::Interface(n)
            | GlobalProp::Namespace(n)
            | GlobalProp::Attr(n)
            | GlobalProp::Op(n)
            | GlobalProp::Other(n) => n,
            GlobalProp::Alias { name, .. } | GlobalProp::LegacyFactory { name, .. } => name,
        }
    }
}

/// How much of the captured Chrome surface the IDL reproduces on its own.
#[derive(Debug, Clone, Default)]
pub struct Coverage {
    /// WebIDL interfaces in the fixture (JS built-ins and aliases excluded).
    pub fixture_interfaces: usize,
    /// Of those, defined in the IDL set.
    pub idl_interfaces: usize,
    /// Interfaces whose every fixture member (prototype + statics) comes
    /// from the IDL with the same kind and `length`.
    pub exact_members: usize,
    /// Of those, interfaces whose IDL-derived order (before using the
    /// fixture) already equals Chrome's.
    pub exact_order: usize,
    /// Prototype + static members in the fixture (WebIDL interfaces).
    pub fixture_members: usize,
    /// Of those, found in the IDL with the same kind.
    pub idl_members: usize,
    /// Operations whose IDL length differs from Chrome's.
    pub length_mismatch: Vec<String>,
    /// `Iface.member` present in Chrome but not derivable from the IDL.
    pub fixture_only: Vec<String>,
    /// Interfaces whose IDL order differs (name only).
    pub order_mismatch: Vec<String>,
    pub window_props: usize,
    /// Window props classified as something other than `Other`.
    pub window_props_resolved: usize,
    pub warnings: Vec<String>,
}

impl Coverage {
    pub fn summary(&self) -> String {
        format!(
            "interfaces: {} in fixture, {} in IDL, {} with exact members, {} with exact IDL order; \
             members: {}/{} from IDL ({} fixture-only, {} length mismatches); \
             window props: {}/{} resolved",
            self.fixture_interfaces,
            self.idl_interfaces,
            self.exact_members,
            self.exact_order,
            self.idl_members,
            self.fixture_members,
            self.fixture_only.len(),
            self.length_mismatch.len(),
            self.window_props_resolved,
            self.window_props,
        )
    }
}

#[derive(Debug, Clone, Default)]
pub struct Table {
    pub interfaces: Vec<Interface>,
    pub namespaces: Vec<Namespace>,
    pub window_props: Vec<GlobalProp>,
    pub window_late_start: usize,
    pub coverage: Coverage,
}

impl Table {
    pub fn interface(&self, name: &str) -> Option<&Interface> {
        self.interfaces.iter().find(|i| i.name == name)
    }
}

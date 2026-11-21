//! Print a [`Table`] as static Rust data for `lb_webapi::desc`.
//!
//! The data holds no pointers (so a position-independent binary needs no
//! relocations for it): strings are `S::new(offset, len)` into `STRINGS`,
//! lists `L::new(start, len)` into one pool per element type.

use std::collections::HashMap;
use std::fmt::Write;

use crate::ir::{
    Attr, AttrTy, ConstV, GlobalProp, Interface, Iterable, Namespace, Op, OpKind, Slot, SymIter,
    Table, WorkerScope,
};

/// The pools the table's strings and lists point into.
#[derive(Default)]
struct Pools {
    strings: String,
    interned: HashMap<String, (usize, usize)>,
    attrs: Vec<String>,
    ops: Vec<String>,
    slots: Vec<String>,
    names: Vec<String>,
    consts: Vec<String>,
    globals: Vec<String>,
    orders: Vec<String>,
}

impl Pools {
    /// `S::new(..)` for `s`, interned.
    fn q(&mut self, s: &str) -> String {
        let (off, len) = match self.interned.get(s) {
            Some(&v) => v,
            None => {
                let v = (self.strings.len(), s.len());
                self.strings.push_str(s);
                self.interned.insert(s.to_owned(), v);
                v
            }
        };
        format!("S::new({off}, {len})")
    }

    fn list(pool: &mut Vec<String>, items: Vec<String>) -> String {
        let start = pool.len();
        let len = items.len();
        pool.extend(items);
        format!("L::new({start}, {len})")
    }
}

fn const_value(v: ConstV) -> String {
    match v {
        ConstV::Bool(b) => format!("ConstValue::Bool({b})"),
        ConstV::Number(n) if n.is_nan() => "ConstValue::Number(f64::NAN)".into(),
        ConstV::Number(n) if n == f64::INFINITY => "ConstValue::Number(f64::INFINITY)".into(),
        ConstV::Number(n) if n == f64::NEG_INFINITY => {
            "ConstValue::Number(f64::NEG_INFINITY)".into()
        }
        ConstV::Number(n) => format!("ConstValue::Number({n:?})"),
    }
}

fn attr(p: &mut Pools, a: &Attr) -> String {
    let mut f = vec![format!("name: {}", p.q(&a.name))];
    if a.readonly {
        f.push("readonly: true".into());
    }
    if a.is_static {
        f.push("is_static: true".into());
    }
    if a.unforgeable {
        f.push("unforgeable: true".into());
    }
    if a.replaceable {
        f.push("replaceable: true".into());
    }
    if a.same_object {
        f.push("same_object: true".into());
    }
    if let Some(fwd) = &a.put_forwards {
        f.push(format!("put_forwards: Some({})", p.q(fwd)));
    }
    if a.on_instance {
        f.push("on_instance: true".into());
    }
    if !a.enumerable {
        f.push("enumerable: false".into());
    }
    if a.late {
        f.push("late: true".into());
    }
    if a.secure_only {
        f.push("secure_context: true".into());
    }
    let ty = match a.ty {
        AttrTy::Other => None,
        AttrTy::Null => Some("Null"),
        AttrTy::Bool => Some("Bool"),
        AttrTy::Number => Some("Number"),
        AttrTy::String => Some("String"),
    };
    if let Some(t) = ty {
        f.push(format!("ty: AttrType::{t}"));
    }
    format!("AttrDesc {{ {}, ..AttrDesc::DEFAULT }}", f.join(", "))
}

fn op(p: &mut Pools, o: &Op) -> String {
    let mut f = vec![format!("name: {}", p.q(&o.name))];
    if o.length != 0 {
        f.push(format!("length: {}", o.length));
    }
    if o.is_static {
        f.push("is_static: true".into());
    }
    if o.unforgeable {
        f.push("unforgeable: true".into());
    }
    if o.on_instance {
        f.push("on_instance: true".into());
    }
    if !o.enumerable {
        f.push("enumerable: false".into());
    }
    if o.late {
        f.push("late: true".into());
    }
    if o.secure_only {
        f.push("secure_context: true".into());
    }
    match o.kind {
        OpKind::Regular => {}
        OpKind::Stringifier => f.push("kind: OpKind::Stringifier".into()),
        OpKind::Iteration => f.push("kind: OpKind::Iteration".into()),
        OpKind::Array(a) => f.push(format!("kind: OpKind::Array(ArrayIntrinsic::{a})")),
    }
    format!("OpDesc {{ {}, ..OpDesc::DEFAULT }}", f.join(", "))
}

fn slots(p: &mut Pools, s: &[Slot]) -> String {
    let parts: Vec<String> = s
        .iter()
        .map(|s| match s {
            Slot::Const(i) => format!("C({i})"),
            Slot::Attr(i) => format!("A({i})"),
            Slot::Op(i) => format!("O({i})"),
        })
        .collect();
    Pools::list(&mut p.slots, parts)
}

fn strs(p: &mut Pools, v: &[String]) -> String {
    let parts: Vec<String> = v.iter().map(|s| p.q(s)).collect();
    Pools::list(&mut p.names, parts)
}

fn consts(p: &mut Pools, v: &[(String, ConstV)]) -> String {
    let parts: Vec<String> = v
        .iter()
        .map(|(n, c)| format!("({}, {})", p.q(n), const_value(*c)))
        .collect();
    Pools::list(&mut p.consts, parts)
}

fn attrs(p: &mut Pools, v: &[Attr]) -> String {
    let parts: Vec<String> = v.iter().map(|a| attr(p, a)).collect();
    Pools::list(&mut p.attrs, parts)
}

fn ops(p: &mut Pools, v: &[Op]) -> String {
    let parts: Vec<String> = v.iter().map(|o| op(p, o)).collect();
    Pools::list(&mut p.ops, parts)
}

fn interface(p: &mut Pools, i: &Interface) -> String {
    let mut f: Vec<String> = vec![format!("name: {}", p.q(&i.name))];
    if let Some(par) = &i.parent {
        f.push(format!("parent: Some({})", p.q(par)));
    }
    if i.exposed != 1 {
        f.push(format!("exposed: Exposed({})", i.exposed));
    }
    if i.is_global {
        f.push("is_global: true".into());
    }
    if !i.legacy_window_alias.is_empty() {
        f.push(format!(
            "legacy_window_alias: {}",
            strs(p, &i.legacy_window_alias)
        ));
    }
    if i.has_constructor {
        f.push("has_constructor: true".into());
    }
    if i.constructor_length != 0 {
        f.push(format!("constructor_length: {}", i.constructor_length));
    }
    if let Some((n, len)) = &i.legacy_factory {
        f.push(format!("legacy_factory: Some({})", p.q(n)));
        if *len != 0 {
            f.push(format!("legacy_factory_length: {len}"));
        }
    }
    if !i.constants.is_empty() {
        f.push(format!("constants: {}", consts(p, &i.constants)));
    }
    if !i.attributes.is_empty() {
        f.push(format!("attributes: {}", attrs(p, &i.attributes)));
    }
    if !i.operations.is_empty() {
        f.push(format!("operations: {}", ops(p, &i.operations)));
    }
    if i.stringifier {
        f.push("stringifier: true".into());
    }
    if let Some(a) = &i.stringifier_attr {
        f.push(format!("stringifier_attr: Some({})", p.q(a)));
    }
    let it = match i.iterable {
        Iterable::None => None,
        Iterable::Value => Some("Iterable::Value".to_owned()),
        Iterable::Pair => Some("Iterable::Pair".to_owned()),
        Iterable::AsyncValue => Some("Iterable::AsyncValue".to_owned()),
        Iterable::AsyncPair => Some("Iterable::AsyncPair".to_owned()),
        Iterable::Maplike { readonly } => {
            Some(format!("Iterable::Maplike {{ readonly: {readonly} }}"))
        }
        Iterable::Setlike { readonly } => {
            Some(format!("Iterable::Setlike {{ readonly: {readonly} }}"))
        }
    };
    if let Some(it) = it {
        f.push(format!("iterable: {it}"));
    }
    for (flag, name) in [
        (i.indexed_getter, "indexed_getter"),
        (i.named_getter, "named_getter"),
        (i.indexed_setter, "indexed_setter"),
        (i.named_setter, "named_setter"),
        (i.named_deleter, "named_deleter"),
        (i.legacy_unforgeable, "legacy_unforgeable"),
        (i.callback_interface, "callback_interface"),
        (i.from_fixture, "from_fixture"),
    ] {
        if flag {
            f.push(format!("{name}: true"));
        }
    }
    if !i.to_string_tag.is_empty() {
        f.push(format!("to_string_tag: {}", p.q(&i.to_string_tag)));
    }
    if !i.proto_order.is_empty() {
        f.push(format!("proto_order: {}", slots(p, &i.proto_order)));
    }
    if !i.static_order.is_empty() {
        f.push(format!("static_order: {}", slots(p, &i.static_order)));
    }
    if !i.instance_order.is_empty() {
        f.push(format!("instance_order: {}", slots(p, &i.instance_order)));
    }
    match &i.iterator {
        SymIter::None => {}
        SymIter::ArrayValues => f.push("iterator: SymbolIterator::ArrayValues".into()),
        SymIter::Alias(a) => {
            let a = p.q(a);
            f.push(format!("iterator: SymbolIterator::Alias({a})"))
        }
    }
    if let Some(a) = &i.async_iterator {
        f.push(format!("async_iterator: Some({})", p.q(a)));
    }
    if i.secure_only {
        f.push("secure_context: true".into());
    }
    if let Some(b) = &i.builtin_parent {
        f.push(format!("builtin_parent: Some({})", p.q(b)));
    }
    if !i.unscopables.is_empty() {
        f.push(format!("unscopables: {}", strs(p, &i.unscopables)));
    }
    let mut s = String::from("InterfaceDesc {\n");
    for field in f {
        let _ = writeln!(s, "        {field},");
    }
    s.push_str("        ..InterfaceDesc::DEFAULT\n    }");
    s
}

fn namespace(p: &mut Pools, n: &Namespace) -> String {
    let name = p.q(&n.name);
    let c = consts(p, &n.constants);
    let a = attrs(p, &n.attributes);
    let o = ops(p, &n.operations);
    let order = slots(p, &n.order);
    let tag = p.q(&n.to_string_tag);
    format!(
        "NamespaceDesc {{\n        name: {name},\n        exposed: Exposed({}),\n        constants: {c},\n        attributes: {a},\n        operations: {o},\n        order: {order},\n        to_string_tag: {tag},\n    }}",
        n.exposed,
    )
}

fn global_prop(p: &mut Pools, g: &GlobalProp) -> String {
    match g {
        GlobalProp::Builtin(n) => format!("G::Builtin({})", p.q(n)),
        GlobalProp::Interface(n) => format!("G::Interface({})", p.q(n)),
        GlobalProp::Alias { name, target } => {
            format!(
                "G::Alias {{ name: {}, target: {} }}",
                p.q(name),
                p.q(target)
            )
        }
        GlobalProp::LegacyFactory { name, iface } => {
            format!(
                "G::LegacyFactory {{ name: {}, iface: {} }}",
                p.q(name),
                p.q(iface)
            )
        }
        GlobalProp::Namespace(n) => format!("G::Namespace({})", p.q(n)),
        GlobalProp::Attr(n) => format!("G::Attr({})", p.q(n)),
        GlobalProp::Op(n) => format!("G::Op({})", p.q(n)),
        GlobalProp::Other(n) => format!("G::Other({})", p.q(n)),
    }
}

fn worker_scope(p: &mut Pools, w: &WorkerScope) -> String {
    let global = p.q(&w.global);
    let items: Vec<String> = w.props.iter().map(|g| global_prop(p, g)).collect();
    let props = Pools::list(&mut p.globals, items);
    let orders: Vec<String> = w
        .overrides
        .iter()
        .map(|o| {
            let iface = p.q(&o.iface);
            let proto = slots(p, &o.proto_order);
            let statics = slots(p, &o.static_order);
            format!(
                "IfaceOrder {{ iface: {iface}, proto_order: {proto}, ctor_at: {}, static_order: {statics} }}",
                o.ctor_at
            )
        })
        .collect();
    let overrides = Pools::list(&mut p.orders, orders);
    format!(
        "WorkerScopeDesc {{\n        global: {global},\n        exposed: Exposed({}),\n        props: {props},\n        late_start: {},\n        overrides: {overrides},\n    }}",
        w.exposed, w.late_start
    )
}

fn pool(s: &mut String, doc: &str, name: &str, ty: &str, items: &[String]) {
    let _ = writeln!(
        s,
        "/// {doc}\npub static {name}: [{ty}; {}] = [",
        items.len()
    );
    for it in items {
        let _ = writeln!(s, "    {it},");
    }
    s.push_str("];\n\n");
}

/// The whole generated file.
pub fn emit(table: &Table, header: &str) -> String {
    let mut p = Pools::default();
    let interfaces: Vec<String> = table
        .interfaces
        .iter()
        .map(|i| interface(&mut p, i))
        .collect();
    let namespaces: Vec<String> = table
        .namespaces
        .iter()
        .map(|n| namespace(&mut p, n))
        .collect();
    let props: Vec<String> = table
        .window_props
        .iter()
        .map(|g| global_prop(&mut p, g))
        .collect();
    let workers: Vec<String> = table
        .workers
        .iter()
        .map(|w| worker_scope(&mut p, w))
        .collect();
    let secure: Vec<String> = table.window_secure_only.iter().map(|n| p.q(n)).collect();
    let gated: Vec<String> = table
        .os_gated
        .iter()
        .map(|(gate, owner, name)| {
            format!(
                "OsGated {{ gate: {}, owner: {}, name: {} }}",
                p.q(gate),
                p.q(owner),
                p.q(name)
            )
        })
        .collect();

    let mut s = String::new();
    for line in header.lines() {
        let _ = writeln!(s, "// {line}");
    }
    s.push_str(
        "\n#![allow(clippy::all)]\n\nuse crate::desc::GlobalProp as G;\nuse crate::desc::Slot::{Attr as A, Const as C, Op as O};\n#[allow(unused_imports)]\nuse crate::desc::*;\n\n",
    );
    s.push_str(
        "/// The whole table.\npub static TABLE: Table = Table {\n    interfaces: &INTERFACES,\n    namespaces: &NAMESPACES,\n    window_props: &WINDOW_PROPS,\n    window_late_start: WINDOW_LATE_START,\n    window_secure_only: &WINDOW_SECURE_ONLY,\n    os_gated: &OS_GATED,\n    workers: &WORKER_SCOPES,\n};\n\n",
    );
    let _ = writeln!(
        s,
        "/// Index into [`WINDOW_PROPS`] where late properties start.\npub const WINDOW_LATE_START: usize = {};\n",
        table.window_late_start
    );
    pool(
        &mut s,
        "Interfaces, parents first.",
        "INTERFACES",
        "InterfaceDesc",
        &interfaces,
    );
    pool(
        &mut s,
        "Namespaces on the window.",
        "NAMESPACES",
        "NamespaceDesc",
        &namespaces,
    );
    pool(
        &mut s,
        "Own properties of the window global, in Chrome's order.",
        "WINDOW_PROPS",
        "GlobalProp",
        &props,
    );
    pool(
        &mut s,
        "Window props that exist only in secure contexts.",
        "WINDOW_SECURE_ONLY",
        "S",
        &secure,
    );
    pool(
        &mut s,
        "Members only some OSes have (`gate`, owner interface or `Window`, name).",
        "OS_GATED",
        "OsGated",
        &gated,
    );
    pool(
        &mut s,
        "Worker global scopes (dedicated, shared).",
        "WORKER_SCOPES",
        "WorkerScopeDesc",
        &workers,
    );
    pool(
        &mut s,
        "Own properties of the worker globals, in Chrome's order.",
        "WORKER_PROPS",
        "GlobalProp",
        &p.globals,
    );
    pool(
        &mut s,
        "Interface orders that differ in a worker scope.",
        "IFACE_ORDERS",
        "IfaceOrder",
        &p.orders,
    );
    pool(
        &mut s,
        "Attributes of all interfaces and namespaces.",
        "ATTRS",
        "AttrDesc",
        &p.attrs,
    );
    pool(
        &mut s,
        "Operations of all interfaces and namespaces.",
        "OPS",
        "OpDesc",
        &p.ops,
    );
    pool(&mut s, "Property orders.", "SLOTS", "Slot", &p.slots);
    pool(
        &mut s,
        "String lists (aliases, unscopables).",
        "NAMES",
        "S",
        &p.names,
    );
    pool(&mut s, "Constants.", "CONSTS", "(S, ConstValue)", &p.consts);
    let _ = writeln!(
        s,
        "/// Every string of the table.\npub static STRINGS: &str = {:?};",
        p.strings
    );
    s
}

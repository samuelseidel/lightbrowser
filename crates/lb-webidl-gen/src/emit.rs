//! Print a [`Table`] as static Rust data for `lb_webapi::desc`.

use std::fmt::Write;

use crate::ir::{
    Attr, AttrTy, ConstV, GlobalProp, Interface, Iterable, Namespace, Op, OpKind, Slot, SymIter,
    Table,
};

fn q(s: &str) -> String {
    format!("{s:?}")
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

fn attr(a: &Attr) -> String {
    let mut f = vec![format!("name: {}", q(&a.name))];
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
    if let Some(p) = &a.put_forwards {
        f.push(format!("put_forwards: Some({})", q(p)));
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

fn op(o: &Op) -> String {
    let mut f = vec![format!("name: {}", q(&o.name))];
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

fn slots(s: &[Slot]) -> String {
    let parts: Vec<String> = s
        .iter()
        .map(|s| match s {
            Slot::Const(i) => format!("C({i})"),
            Slot::Attr(i) => format!("A({i})"),
            Slot::Op(i) => format!("O({i})"),
        })
        .collect();
    format!("&[{}]", parts.join(", "))
}

fn strs(v: &[String]) -> String {
    let parts: Vec<String> = v.iter().map(|s| q(s)).collect();
    format!("&[{}]", parts.join(", "))
}

fn list<T>(items: &[T], f: impl Fn(&T) -> String, indent: &str) -> String {
    if items.is_empty() {
        return "&[]".into();
    }
    let mut s = String::from("&[\n");
    for it in items {
        let _ = writeln!(s, "{indent}    {},", f(it));
    }
    let _ = write!(s, "{indent}]");
    s
}

fn interface(i: &Interface) -> String {
    let ind = "        ";
    let mut f: Vec<String> = vec![format!("name: {}", q(&i.name))];
    if let Some(p) = &i.parent {
        f.push(format!("parent: Some({})", q(p)));
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
            strs(&i.legacy_window_alias)
        ));
    }
    if i.has_constructor {
        f.push("has_constructor: true".into());
    }
    if i.constructor_length != 0 {
        f.push(format!("constructor_length: {}", i.constructor_length));
    }
    if let Some((n, len)) = &i.legacy_factory {
        f.push(format!("legacy_factory: Some({})", q(n)));
        if *len != 0 {
            f.push(format!("legacy_factory_length: {len}"));
        }
    }
    if !i.constants.is_empty() {
        f.push(format!(
            "constants: {}",
            list(
                &i.constants,
                |(n, v)| format!("({}, {})", q(n), const_value(*v)),
                ind
            )
        ));
    }
    if !i.attributes.is_empty() {
        f.push(format!("attributes: {}", list(&i.attributes, attr, ind)));
    }
    if !i.operations.is_empty() {
        f.push(format!("operations: {}", list(&i.operations, op, ind)));
    }
    if i.stringifier {
        f.push("stringifier: true".into());
    }
    if let Some(a) = &i.stringifier_attr {
        f.push(format!("stringifier_attr: Some({})", q(a)));
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
        f.push(format!("to_string_tag: {}", q(&i.to_string_tag)));
    }
    if !i.proto_order.is_empty() {
        f.push(format!("proto_order: {}", slots(&i.proto_order)));
    }
    if !i.static_order.is_empty() {
        f.push(format!("static_order: {}", slots(&i.static_order)));
    }
    if !i.instance_order.is_empty() {
        f.push(format!("instance_order: {}", slots(&i.instance_order)));
    }
    match &i.iterator {
        SymIter::None => {}
        SymIter::ArrayValues => f.push("iterator: SymbolIterator::ArrayValues".into()),
        SymIter::Alias(a) => f.push(format!("iterator: SymbolIterator::Alias({})", q(a))),
    }
    if let Some(a) = &i.async_iterator {
        f.push(format!("async_iterator: Some({})", q(a)));
    }
    if i.secure_only {
        f.push("secure_context: true".into());
    }
    if let Some(b) = &i.builtin_parent {
        f.push(format!("builtin_parent: Some({})", q(b)));
    }
    if !i.unscopables.is_empty() {
        f.push(format!("unscopables: {}", strs(&i.unscopables)));
    }
    let mut s = String::from("InterfaceDesc {\n");
    for field in f {
        let _ = writeln!(s, "        {field},");
    }
    s.push_str("        ..InterfaceDesc::DEFAULT\n    }");
    s
}

fn namespace(n: &Namespace) -> String {
    let ind = "        ";
    format!(
        "NamespaceDesc {{\n        name: {},\n        exposed: Exposed({}),\n        constants: {},\n        attributes: {},\n        operations: {},\n        order: {},\n        to_string_tag: {},\n    }}",
        q(&n.name),
        n.exposed,
        list(
            &n.constants,
            |(n, v)| format!("({}, {})", q(n), const_value(*v)),
            ind
        ),
        list(&n.attributes, attr, ind),
        list(&n.operations, op, ind),
        slots(&n.order),
        q(&n.to_string_tag),
    )
}

fn global(p: &GlobalProp) -> String {
    match p {
        GlobalProp::Builtin(n) => format!("G::Builtin({})", q(n)),
        GlobalProp::Interface(n) => format!("G::Interface({})", q(n)),
        GlobalProp::Alias { name, target } => {
            format!("G::Alias {{ name: {}, target: {} }}", q(name), q(target))
        }
        GlobalProp::LegacyFactory { name, iface } => {
            format!(
                "G::LegacyFactory {{ name: {}, iface: {} }}",
                q(name),
                q(iface)
            )
        }
        GlobalProp::Namespace(n) => format!("G::Namespace({})", q(n)),
        GlobalProp::Attr(n) => format!("G::Attr({})", q(n)),
        GlobalProp::Op(n) => format!("G::Op({})", q(n)),
        GlobalProp::Other(n) => format!("G::Other({})", q(n)),
    }
}

/// The whole generated file.
pub fn emit(table: &Table, header: &str) -> String {
    let mut s = String::new();
    for line in header.lines() {
        let _ = writeln!(s, "// {line}");
    }
    s.push_str(
        "\n#![allow(clippy::all)]\n\nuse crate::desc::GlobalProp as G;\nuse crate::desc::Slot::{Attr as A, Const as C, Op as O};\n#[allow(unused_imports)]\nuse crate::desc::*;\n\n",
    );
    s.push_str(
        "/// The whole table.\npub static TABLE: Table = Table {\n    interfaces: INTERFACES,\n    namespaces: NAMESPACES,\n    window_props: WINDOW_PROPS,\n    window_late_start: WINDOW_LATE_START,\n    window_secure_only: WINDOW_SECURE_ONLY,\n    os_gated: OS_GATED,\n};\n\n",
    );
    let _ = writeln!(
        s,
        "/// Index into [`WINDOW_PROPS`] where late properties start.\npub const WINDOW_LATE_START: usize = {};\n",
        table.window_late_start
    );
    s.push_str("/// Interfaces, parents first.\npub static INTERFACES: &[InterfaceDesc] = &[\n");
    for i in &table.interfaces {
        let _ = writeln!(s, "    {},", interface(i));
    }
    s.push_str(
        "];\n\n/// Namespaces on the window.\npub static NAMESPACES: &[NamespaceDesc] = &[\n",
    );
    for n in &table.namespaces {
        let _ = writeln!(s, "    {},", namespace(n));
    }
    s.push_str("];\n\n/// Own properties of the window global, in Chrome's order.\npub static WINDOW_PROPS: &[GlobalProp] = &[\n");
    for p in &table.window_props {
        let _ = writeln!(s, "    {},", global(p));
    }
    s.push_str("];\n\n/// Window props that exist only in secure contexts.\npub static WINDOW_SECURE_ONLY: &[&str] = &[\n");
    for n in &table.window_secure_only {
        let _ = writeln!(s, "    {},", q(n));
    }
    s.push_str("];\n\n/// Members only some OSes have (`gate`, owner interface or `Window`, name).\npub static OS_GATED: &[OsGated] = &[\n");
    for (gate, owner, name) in &table.os_gated {
        let _ = writeln!(
            s,
            "    OsGated {{ gate: {}, owner: {}, name: {} }},",
            q(gate),
            q(owner),
            q(name)
        );
    }
    s.push_str("];\n");
    s
}

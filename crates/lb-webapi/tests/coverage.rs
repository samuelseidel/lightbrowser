//! How much of Chrome's Web API surface is bound to real host code and how
//! much is a stub (`docs/PLAN.md` M2 asks for the number).
//!
//! `tests/install.rs` checks shapes (names, order, descriptors), and a stub
//! has all of those. The installer (`install.rs`) gives every member of the
//! generated table a registered implementation or a stub: a getter that
//! returns a type-shaped default, an operation that throws `NotSupportedError`
//! ("Not supported."), a constructor that throws "Illegal constructor". This
//! test asks the registry the way the installer does and counts.
//!
//! Counted per (interface, member), as Chrome defines them (a mixin member is
//! one per interface that includes it):
//!
//! - an **operation** is real when the registry has a method for it (static
//!   operations: a static method), or, for `toString` of a `stringifier
//!   attribute`, the attribute's getter. Constants and the operations that
//!   are `Array.prototype` functions (`OpKind::Array`, no host code) are not
//!   counted; the latter are printed on their own;
//! - an **attribute** is real when the registry has its getter (static
//!   attributes: a static getter), as `attr_member` looks it up;
//! - an **interface** is real when at least one of its members is: a
//!   constructor, a legacy factory, an attribute getter, an operation, an
//!   indexed or a named getter. Interfaces with nothing to implement
//!   (callback interfaces with constants only) are left out of the total.
//!
//! The registry is the page layer's: `rt::Runtime::new` plus what
//! `Bindings::define` adds before it freezes (the `DOMException` defaults and
//! the `window`, `self`, `frames`, `top`, `parent` getters). Nothing runs
//! JavaScript, so this takes milliseconds.
//!
//! The floors below are today's numbers: a registration that disappears
//! fails the test, and an improvement passes (raise the floor with it). The
//! line `web api coverage: N/M operations, ...` goes to the real stderr, so
//! every `cargo test` log has it (`docs/PLAN.md`: "track coverage as a number
//! in CI"). `LB_COVERAGE_DUMP=file` writes one line per interface to see the
//! gaps.
#![cfg(feature = "chrome")]

use std::fmt::Write as _;
use std::io::Write as _;

use lb_persona::BrowserFamily;
use lb_webapi::desc::{AttrDesc, AttrType, InterfaceDesc, OpDesc, OpKind, Table};
use lb_webapi::{ImplKind, Registry, host_fn, register_dom_exception, table_for};

/// Real operations, attributes and interfaces on 2026-11-16 (Chrome 153
/// table): raise them when an implementation lands.
const FLOOR_OPERATIONS: usize = 1143; // of 2458
const FLOOR_ATTRIBUTES: usize = 2269; // of 4504
const FLOOR_INTERFACES: usize = 394; // of 893

/// The registry `Bindings::define` works from.
fn full_registry() -> Registry {
    let mut reg = Registry::new();
    let _rt = lb_webapi::rt::Runtime::new(&mut reg);
    // `define_scope` adds these to its copy before it freezes it.
    register_dom_exception(&mut reg);
    for name in ["window", "self", "frames", "top", "parent"] {
        if reg.get("Window", name, ImplKind::Getter).is_none() {
            reg.getter("Window", name, host_fn(|_| Ok(())));
        }
    }
    reg
}

/// `real` of `total`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Ratio {
    real: usize,
    total: usize,
}

impl Ratio {
    fn add(&mut self, real: bool) {
        self.real += usize::from(real);
        self.total += 1;
    }
    fn is_empty(&self) -> bool {
        self.total == 0
    }
    fn full(&self) -> bool {
        self.real == self.total
    }
}

impl std::fmt::Display for Ratio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.real, self.total)
    }
}

/// One interface (or namespace) of the table.
#[derive(Debug, Default)]
struct Row {
    name: &'static str,
    ops: Ratio,
    attrs: Ratio,
    /// Writable attributes with a real setter.
    setters: Ratio,
    /// Constructor or legacy factory (1 or 2 of them).
    ctors: Ratio,
    /// Indexed or named getter.
    collections: Ratio,
    /// Real operations, attributes, constructors and collections other than
    /// event-handler attributes (`on*`, nullable), which one generic
    /// registration may supply for many interfaces.
    real_beyond_handlers: usize,
}

impl Row {
    /// Something to implement at all.
    fn implementable(&self) -> bool {
        !(self.ops.is_empty()
            && self.attrs.is_empty()
            && self.ctors.is_empty()
            && self.collections.is_empty())
    }
    /// At least one real member.
    fn touched(&self) -> bool {
        self.ops.real + self.attrs.real + self.ctors.real + self.collections.real > 0
    }
    fn complete(&self) -> bool {
        self.ops.full() && self.attrs.full() && self.ctors.full() && self.collections.full()
    }
}

fn op_real(reg: &Registry, iface: &'static str, d: Option<&InterfaceDesc>, o: &OpDesc) -> bool {
    let kind = if o.is_static {
        ImplKind::StaticMethod
    } else {
        ImplKind::Method
    };
    reg.get_declared(iface, o.name.get(), kind).is_some()
        // `toString` of `stringifier attribute x` is x's getter.
        || (o.kind == OpKind::Stringifier
            && d.and_then(|d| d.stringifier_attr).is_some_and(|a| {
                reg.get_declared(iface, a.get(), ImplKind::Getter).is_some()
            }))
}

fn attr_real(reg: &Registry, iface: &'static str, a: &AttrDesc) -> (bool, Option<bool>) {
    let (gk, sk) = if a.is_static {
        (ImplKind::StaticGetter, ImplKind::StaticSetter)
    } else {
        (ImplKind::Getter, ImplKind::Setter)
    };
    let getter = reg.get_declared(iface, a.name.get(), gk).is_some();
    // `[PutForwards]` and `[Replaceable]` setters are the installer's own.
    let setter = (!a.readonly).then(|| reg.get_declared(iface, a.name.get(), sk).is_some());
    (getter, setter)
}

/// Members of `iface`: its operations and attributes against the registry.
fn members(
    reg: &Registry,
    iface: &'static str,
    d: Option<&InterfaceDesc>,
    ops: &'static [OpDesc],
    attrs: &'static [AttrDesc],
    row: &mut Row,
    intrinsics: &mut usize,
) {
    for o in ops {
        if matches!(o.kind, OpKind::Array(_)) {
            *intrinsics += 1;
            continue;
        }
        let real = op_real(reg, iface, d, o);
        row.ops.add(real);
        row.real_beyond_handlers += usize::from(real);
    }
    for a in attrs {
        let (getter, setter) = attr_real(reg, iface, a);
        row.attrs.add(getter);
        let handler = a.ty == AttrType::Null && a.name.get().starts_with("on");
        row.real_beyond_handlers += usize::from(getter && !handler);
        if let Some(s) = setter {
            row.setters.add(s);
        }
    }
}

#[derive(Debug, Default)]
struct Coverage {
    /// The table's interfaces, with or without anything to implement.
    rows: Vec<Row>,
    /// The table's namespaces (`CSS`): their members count as operations
    /// and attributes, the namespaces themselves are not interfaces.
    namespace_rows: Vec<Row>,
    /// `OpKind::Array` operations: the engine's, no host code.
    intrinsics: usize,
}

impl Coverage {
    fn ops(&self) -> Ratio {
        self.sum(|r| r.ops)
    }
    fn attrs(&self) -> Ratio {
        self.sum(|r| r.attrs)
    }
    fn setters(&self) -> Ratio {
        self.sum(|r| r.setters)
    }
    fn ctors(&self) -> Ratio {
        self.sum(|r| r.ctors)
    }
    /// Operations, attributes, ... of interfaces and namespaces together.
    fn sum(&self, f: impl Fn(&Row) -> Ratio) -> Ratio {
        let all = self.rows.iter().chain(&self.namespace_rows);
        all.map(f).fold(Ratio::default(), |a, b| Ratio {
            real: a.real + b.real,
            total: a.total + b.total,
        })
    }
    /// Interfaces with something to implement: how many have a real member,
    /// and how many have only real members.
    fn interfaces(&self) -> (Ratio, usize) {
        let mut touched = Ratio::default();
        let mut complete = 0;
        for r in self.rows.iter().filter(|r| r.implementable()) {
            touched.add(r.touched());
            complete += usize::from(r.complete());
        }
        (touched, complete)
    }
    /// Interfaces with a real member that is not an event-handler attribute.
    fn interfaces_beyond_handlers(&self) -> usize {
        self.rows
            .iter()
            .filter(|r| r.real_beyond_handlers > 0)
            .count()
    }
    /// Namespaces with something to implement (the `GPU*Usage` ones only
    /// hold constants): how many have a real member.
    fn namespaces(&self) -> Ratio {
        let mut n = Ratio::default();
        for r in self.namespace_rows.iter().filter(|r| r.implementable()) {
            n.add(r.touched());
        }
        n
    }
}

fn coverage(table: &'static Table, reg: &Registry) -> Coverage {
    let mut c = Coverage::default();
    for n in table.namespaces {
        let mut row = Row {
            name: n.name.get(),
            ..Default::default()
        };
        members(
            reg,
            n.name.get(),
            None,
            n.operations.get(),
            n.attributes.get(),
            &mut row,
            &mut c.intrinsics,
        );
        c.namespace_rows.push(row);
    }
    for d in table.interfaces {
        let name = d.name.get();
        let mut row = Row {
            name,
            ..Default::default()
        };
        members(
            reg,
            name,
            Some(d),
            d.operations.get(),
            d.attributes.get(),
            &mut row,
            &mut c.intrinsics,
        );
        if d.has_constructor {
            row.ctors
                .add(reg.get(name, "", ImplKind::Constructor).is_some());
        }
        if let Some(f) = d.legacy_factory {
            row.ctors
                .add(reg.get(name, f.get(), ImplKind::LegacyFactory).is_some());
        }
        if d.indexed_getter {
            row.collections.add(reg.indexed_getter_for(name).is_some());
        }
        if d.named_getter {
            row.collections.add(reg.named_getter_for(name).is_some());
        }
        row.real_beyond_handlers += row.ctors.real + row.collections.real;
        c.rows.push(row);
    }
    c
}

/// One line per interface and namespace: `name ops attrs setters ctors
/// collections`.
fn dump(c: &Coverage) -> String {
    let mut s = String::from(
        "# name ops attributes setters constructors indexed/named real-beyond-event-handlers\n",
    );
    for r in c.rows.iter().chain(&c.namespace_rows) {
        let _ = writeln!(
            s,
            "{} {} {} {} {} {} {}",
            r.name, r.ops, r.attrs, r.setters, r.ctors, r.collections, r.real_beyond_handlers
        );
    }
    s
}

#[test]
fn web_api_coverage() {
    let table = table_for(BrowserFamily::Chrome).expect("the chrome table");
    let c = coverage(table, &full_registry());
    let (ops, attrs) = (c.ops(), c.attrs());
    let (ifaces, complete) = c.interfaces();
    // The real stderr, which libtest does not capture: the number is in
    // every `cargo test` log (CI), not only with `--nocapture`.
    let mut log = std::io::stderr();
    let _ = writeln!(
        log,
        "web api coverage: {}/{} operations, {}/{} attributes, {}/{} interfaces",
        ops.real, ops.total, attrs.real, attrs.total, ifaces.real, ifaces.total
    );
    let _ = writeln!(
        log,
        "  more: {} of {} writable attributes have a real setter, {} of {} constructors and \
         factories are real, {complete} interfaces are complete (every member real), \
         {} have a real member beyond event handlers, {} of {} namespaces have a real member; \
         {} interfaces in the table, {} with nothing to implement; {} operations are \
         Array.prototype functions (no host code, not counted)",
        c.setters().real,
        c.setters().total,
        c.ctors().real,
        c.ctors().total,
        c.interfaces_beyond_handlers(),
        c.namespaces().real,
        c.namespaces().total,
        c.rows.len(),
        c.rows.len() - ifaces.total,
        c.intrinsics,
    );
    if let Ok(path) = std::env::var("LB_COVERAGE_DUMP") {
        std::fs::write(path, dump(&c)).unwrap();
    }
    assert!(
        ops.real >= FLOOR_OPERATIONS
            && attrs.real >= FLOOR_ATTRIBUTES
            && ifaces.real >= FLOOR_INTERFACES,
        "implementation coverage fell: {}/{} operations (floor {FLOOR_OPERATIONS}), {}/{} \
         attributes (floor {FLOOR_ATTRIBUTES}), {}/{} interfaces (floor {FLOOR_INTERFACES})",
        ops.real,
        ops.total,
        attrs.real,
        attrs.total,
        ifaces.real,
        ifaces.total
    );
}

/// The counting itself, on a registry the test controls: a stub is a stub,
/// and a registered member is real.
#[test]
fn an_empty_registry_has_nothing_real_and_a_registration_counts() {
    let table = table_for(BrowserFamily::Chrome).unwrap();
    let empty = coverage(table, &Registry::new());
    assert_eq!(empty.ops().real, 0);
    assert_eq!(empty.attrs().real, 0);
    assert_eq!(empty.interfaces().0.real, 0);
    assert!(empty.ops().total > 1000 && empty.attrs().total > 1000);

    // Document.getElementById: one more real operation and interface.
    let mut reg = Registry::new();
    reg.method("Document", "getElementById", host_fn(|_| Ok(())));
    reg.getter("Document", "title", host_fn(|_| Ok(())));
    let one = coverage(table, &reg);
    assert_eq!(one.ops().real, 1);
    assert_eq!(one.attrs().real, 1);
    assert_eq!(one.interfaces().0.real, 1);
    assert_eq!(one.ops().total, empty.ops().total, "the totals do not move");

    // The full registry is a strict superset of both.
    let full = coverage(table, &full_registry());
    assert!(full.ops().real > one.ops().real && full.attrs().real > one.attrs().real);
}

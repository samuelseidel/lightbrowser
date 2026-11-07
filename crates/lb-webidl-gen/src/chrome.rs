//! The Chrome pass: turn the merged IDL model into binding tables that
//! match a captured Chrome surface (`tests/fixtures/chromium_153_surface.json`).
//!
//! The IDL says what each member is (attribute, operation, constant, its
//! extended attributes and types). The fixture is the arbiter of what
//! Chrome actually exposes and in which order: `[RuntimeEnabled]` members
//! that Chrome 153 does not ship are dropped, and members are ordered as
//! the fixture lists them. Features seen enabled in the fixture are then
//! used to filter what the fixture does not cover (unforgeable instance
//! members, worker-only interfaces, namespaces).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde_json::Value;

use crate::ir::{
    Attr, AttrTy, ConstV, Coverage, GlobalProp, IfaceOrder, Interface, Iterable, Namespace, Op,
    OpKind, Slot, SymIter, Table, WorkerScope,
};
use crate::model::{Interface as IdlInterface, MemberInfo, Model};
use crate::parse::{DefKind, ExtValue, MemberKind, Special, Type, required_args};

/// V8 globals that are not constructors with a prototype in the fixture's
/// `interfaces` map.
const V8_GLOBALS: &[&str] = &[
    "Infinity",
    "NaN",
    "undefined",
    "globalThis",
    "JSON",
    "Math",
    "Intl",
    "Atomics",
    "Reflect",
    "console",
    "Temporal",
    "WebAssembly",
    "parseFloat",
    "parseInt",
    "decodeURI",
    "decodeURIComponent",
    "encodeURI",
    "encodeURIComponent",
    "escape",
    "unescape",
    "eval",
    "isFinite",
    "isNaN",
    "Proxy",
];

/// Exposure bits, same values as `lb_webapi::desc::Exposed`.
pub mod exposed {
    pub const WINDOW: u16 = 1;
    pub const DEDICATED_WORKER: u16 = 2;
    pub const SHARED_WORKER: u16 = 4;
    pub const SERVICE_WORKER: u16 = 8;
    pub const WORKLET: u16 = 16;
    pub const SHADOW_REALM: u16 = 32;
    pub const WORKER: u16 = 2 | 4 | 8;
    pub const ALL: u16 = 63;
}

fn exposure_bits(names: &[String]) -> u16 {
    let mut bits = 0;
    for n in names {
        bits |= match n.as_str() {
            "*" => exposed::ALL,
            "Window" => exposed::WINDOW,
            "Worker" => exposed::WORKER,
            "DedicatedWorker" => exposed::DEDICATED_WORKER,
            "SharedWorker" => exposed::SHARED_WORKER,
            "ServiceWorker" => exposed::SERVICE_WORKER,
            "ShadowRealm" => exposed::SHADOW_REALM,
            n if n.ends_with("Worklet") => exposed::WORKLET,
            _ => 0,
        };
    }
    bits
}

/// A captured property: name, descriptor code (`gsec`, `fwec`, `ve`, ...),
/// function length.
#[derive(Debug, Clone)]
struct FixProp {
    name: String,
    code: String,
    len: Option<u8>,
}

impl FixProp {
    fn kind(&self) -> char {
        self.code.chars().next().unwrap_or('?')
    }
    fn flag(&self, c: char) -> bool {
        self.code[1..].contains(c)
    }
}

fn fix_props(v: &Value) -> Vec<FixProp> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| {
            let a = e.as_array()?;
            Some(FixProp {
                name: a.first()?.as_str()?.to_owned(),
                code: a.get(1)?.as_str()?.to_owned(),
                len: a.get(2).and_then(Value::as_u64).map(|n| n as u8),
            })
        })
        .collect()
}

struct FixIface {
    length: u8,
    statics: Vec<FixProp>,
    proto: Vec<FixProp>,
    symbols: Vec<String>,
    parent: Option<String>,
    to_string_tag: Option<String>,
    ctor_to_string: Option<String>,
}

struct Fixture {
    interfaces: HashMap<String, FixIface>,
    window: Vec<FixProp>,
    worker: HashSet<String>,
}

fn load_interfaces(v: &Value) -> HashMap<String, FixIface> {
    let mut interfaces = HashMap::new();
    for (name, f) in v.as_object().into_iter().flatten() {
        interfaces.insert(
            name.clone(),
            FixIface {
                length: f["length"].as_u64().unwrap_or(0) as u8,
                statics: fix_props(&f["static"]),
                proto: fix_props(&f["proto"]),
                symbols: f["protoSymbols"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|s| s.as_str().map(str::to_owned))
                    .collect(),
                parent: f["parent"].as_str().map(str::to_owned),
                to_string_tag: f["toStringTag"].as_str().map(str::to_owned),
                ctor_to_string: f["ctorToString"].as_str().map(str::to_owned),
            },
        );
    }
    interfaces
}

fn load_fixture(v: &Value) -> Result<Fixture, String> {
    if v["interfaces"].as_object().is_none() {
        return Err("fixture: no interfaces".into());
    }
    let interfaces = load_interfaces(&v["interfaces"]);
    let window = fix_props(&v["windowProps"]);
    if window.is_empty() {
        return Err("fixture: no windowProps".into());
    }
    let worker = v["worker"]["self"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s.as_str().map(str::to_owned))
        .collect();
    Ok(Fixture {
        interfaces,
        window,
        worker,
    })
}

/// One worker global scope of the worker fixture
/// (`tests/fixtures/chromium_153_worker_surface.json`).
struct WorkerFix {
    /// The global interface (first entry of the prototype chain).
    global: String,
    exposed: u16,
    props: Vec<FixProp>,
    interfaces: HashMap<String, FixIface>,
}

fn load_worker_fixture(v: &Value) -> Result<Vec<WorkerFix>, String> {
    let mut out = Vec::new();
    for (key, bit) in [
        ("dedicated", exposed::DEDICATED_WORKER),
        ("shared", exposed::SHARED_WORKER),
        ("service", exposed::SERVICE_WORKER),
    ] {
        let w = &v[key];
        let global = w["protoChain"][0]
            .as_str()
            .ok_or_else(|| format!("worker fixture: {key} has no protoChain"))?
            .to_owned();
        let props = fix_props(&w["globalProps"]);
        if props.is_empty() {
            return Err(format!("worker fixture: {key} has no globalProps"));
        }
        out.push(WorkerFix {
            global,
            exposed: bit,
            props,
            interfaces: load_interfaces(&w["interfaces"]),
        });
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    Proto,
    Static,
    Instance,
}

#[derive(Debug, Clone)]
enum CandKind {
    Const(ConstV),
    Attr(Attr),
    Op(Op),
}

impl CandKind {
    fn code(&self) -> char {
        match self {
            CandKind::Const(_) => 'v',
            CandKind::Attr(_) => 'g',
            CandKind::Op(_) => 'f',
        }
    }
}

/// One IDL-derived property, before matching against the fixture.
#[derive(Debug, Clone)]
struct Cand {
    name: String,
    place: Place,
    kind: CandKind,
    features: Vec<String>,
    exposed: u16,
    /// (group, index): the IDL-only order guess.
    rank: (u8, usize),
    unscopable: bool,
    /// Installed per context after the template (narrower exposure, secure
    /// context, isolation): comes after `constructor` in Chrome.
    conditional: bool,
}

/// Everything derived from one interface's IDL.
struct Derived {
    cands: Vec<Cand>,
    has_constructor: bool,
    constructor_length: u8,
    iterable: Iterable,
    indexed_getter: bool,
    named_getter: bool,
    indexed_setter: bool,
    named_setter: bool,
    named_deleter: bool,
    stringifier: bool,
    stringifier_attr: Option<String>,
}

fn attr_ty(model: &Model, t: &Type) -> AttrTy {
    if t.nullable {
        return AttrTy::Null;
    }
    match t.name.as_str() {
        "EventHandler" | "OnErrorEventHandler" | "OnBeforeUnloadEventHandler" => AttrTy::Null,
        "boolean" => AttrTy::Bool,
        "byte"
        | "octet"
        | "short"
        | "unsigned short"
        | "long"
        | "unsigned long"
        | "long long"
        | "unsigned long long"
        | "float"
        | "unrestricted float"
        | "double"
        | "unrestricted double"
        | "DOMHighResTimeStamp"
        | "EpochTimeStamp" => AttrTy::Number,
        "DOMString" | "USVString" | "ByteString" | "CSSOMString" => AttrTy::String,
        n if model.enums.contains(n) => AttrTy::String,
        _ => AttrTy::Other,
    }
}

fn parse_const(v: &str) -> ConstV {
    match v {
        "true" => ConstV::Bool(true),
        "false" => ConstV::Bool(false),
        "Infinity" => ConstV::Number(f64::INFINITY),
        "-Infinity" => ConstV::Number(f64::NEG_INFINITY),
        "NaN" => ConstV::Number(f64::NAN),
        _ => {
            let (neg, body) = match v.strip_prefix('-') {
                Some(b) => (true, b),
                None => (false, v),
            };
            let n = if let Some(hex) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
                u64::from_str_radix(hex, 16)
                    .map(|n| n as f64)
                    .unwrap_or(0.0)
            } else if body.len() > 1
                && body.starts_with('0')
                && body.bytes().all(|b| b.is_ascii_digit())
            {
                u64::from_str_radix(&body[1..], 8)
                    .map(|n| n as f64)
                    .unwrap_or(0.0)
            } else {
                body.parse::<f64>().unwrap_or(0.0)
            };
            ConstV::Number(if neg { -n } else { n })
        }
    }
}

fn op(name: &str, length: usize, kind: OpKind) -> Op {
    Op {
        name: name.to_owned(),
        length: length.min(255) as u8,
        is_static: false,
        unforgeable: false,
        on_instance: false,
        enumerable: true,
        late: false,
        kind,
        secure_only: false,
    }
}

/// Derive candidates from the IDL of one interface or namespace.
fn derive(model: &Model, iface: &IdlInterface) -> Derived {
    let is_global = iface.ext.has("Global");
    let iface_unforgeable = iface.ext.has("LegacyUnforgeable");
    let iface_exposed = {
        let e = exposure_bits(&iface.ext.idents("Exposed"));
        if e == 0 { exposed::WINDOW } else { e }
    };
    let namespace = iface.kind == DefKind::Namespace;
    let mut d = Derived {
        cands: Vec::new(),
        has_constructor: false,
        constructor_length: 255,
        iterable: Iterable::None,
        indexed_getter: false,
        named_getter: false,
        indexed_setter: false,
        named_setter: false,
        named_deleter: false,
        stringifier: false,
        stringifier_attr: None,
    };
    // Operations grouped by (name, static) in first-declaration order.
    let mut ops: Vec<(Op, Vec<String>, u16, bool, bool)> = Vec::new();
    let mut idx = 0usize;
    let exposure = |m: &MemberInfo| m.exposed().map_or(iface_exposed, |e| exposure_bits(&e));
    let iface_secure = iface.ext.has("SecureContext");
    let cond = |m: &MemberInfo| {
        exposure(m) & iface_exposed != iface_exposed
            || (!iface_secure && m.ext_has("SecureContext"))
            || m.ext_has("CrossOriginIsolated")
            || m.ext_has("IsolatedContext")
    };

    for m in &iface.members {
        idx += 1;
        let features = m.features();
        let exp = exposure(m);
        let unscopable = m.ext_has("Unscopable");
        let unforgeable = iface_unforgeable || m.ext_has("LegacyUnforgeable");
        match &m.member.kind {
            MemberKind::Const { value } => {
                let v = parse_const(value);
                for place in [Place::Static, Place::Proto] {
                    if namespace && place == Place::Proto {
                        continue;
                    }
                    d.cands.push(Cand {
                        name: m.member.name.clone(),
                        place,
                        kind: CandKind::Const(v),
                        features: features.clone(),
                        exposed: exp,
                        rank: (3, idx),
                        unscopable,
                        conditional: cond(m),
                    });
                }
            }
            MemberKind::Attribute {
                readonly,
                is_static,
                stringifier,
                ty,
                ..
            } => {
                let place = if *is_static || namespace {
                    Place::Static
                } else if unforgeable || is_global {
                    Place::Instance
                } else {
                    Place::Proto
                };
                let put_forwards = m.member.ext.ident("PutForwards").map(str::to_owned);
                let attr = Attr {
                    name: m.member.name.clone(),
                    readonly: *readonly,
                    is_static: *is_static,
                    unforgeable,
                    replaceable: m.member.ext.has("Replaceable"),
                    same_object: m.member.ext.has("SameObject"),
                    put_forwards,
                    on_instance: place == Place::Instance,
                    enumerable: !m.member.ext.has("NotEnumerable"),
                    late: false,
                    ty: attr_ty(model, ty),
                    secure_only: false,
                };
                if *stringifier {
                    d.stringifier = true;
                    d.stringifier_attr = Some(m.member.name.clone());
                }
                d.cands.push(Cand {
                    name: m.member.name.clone(),
                    place,
                    kind: CandKind::Attr(attr),
                    features,
                    exposed: exp,
                    rank: (if *stringifier { 2 } else { 1 }, idx),
                    unscopable,
                    conditional: cond(m),
                });
            }
            MemberKind::Operation {
                is_static,
                specials,
                stringifier,
                args,
                ..
            } => {
                let index_arg = args
                    .first()
                    .is_some_and(|a| a.ty.name.contains("long") || a.ty.name == "short");
                for s in specials {
                    match (s, index_arg) {
                        (Special::Getter, true) => d.indexed_getter = true,
                        (Special::Getter, false) => d.named_getter = true,
                        (Special::Setter, true) => d.indexed_setter = true,
                        (Special::Setter, false) => d.named_setter = true,
                        (Special::Deleter, _) => d.named_deleter = true,
                        (Special::LegacyCaller, _) => {}
                    }
                }
                if *stringifier && m.member.name.is_empty() {
                    d.stringifier = true;
                }
                if m.member.name.is_empty() {
                    continue;
                }
                let st = *is_static || namespace;
                let len = required_args(args);
                if let Some(e) = ops
                    .iter_mut()
                    .find(|(o, ..)| o.name == m.member.name && o.is_static == st)
                {
                    e.0.length = e.0.length.min(len.min(255) as u8);
                    continue;
                }
                let mut o = op(&m.member.name, len, OpKind::Regular);
                o.is_static = st;
                o.unforgeable = unforgeable;
                ops.push((o, features, exp, unscopable, cond(m)));
            }
            MemberKind::Constructor { args } => {
                d.has_constructor = true;
                d.constructor_length = d.constructor_length.min(required_args(args) as u8);
            }
            MemberKind::Stringifier => d.stringifier = true,
            MemberKind::Iterable { pair, is_async } => {
                d.iterable = match (pair, is_async) {
                    (false, false) => Iterable::Value,
                    (true, false) => Iterable::Pair,
                    (false, true) => Iterable::AsyncValue,
                    (true, true) => Iterable::AsyncPair,
                };
            }
            MemberKind::Maplike { readonly } => {
                d.iterable = Iterable::Maplike {
                    readonly: *readonly,
                }
            }
            MemberKind::Setlike { readonly } => {
                d.iterable = Iterable::Setlike {
                    readonly: *readonly,
                }
            }
        }
    }
    if d.constructor_length == 255 {
        d.constructor_length = 0;
    }

    for (i, (mut o, features, exp, unscopable, conditional)) in ops.into_iter().enumerate() {
        let place = if o.is_static {
            Place::Static
        } else if o.unforgeable || is_global {
            Place::Instance
        } else {
            Place::Proto
        };
        o.on_instance = place == Place::Instance;
        d.cands.push(Cand {
            name: o.name.clone(),
            place,
            kind: CandKind::Op(o),
            features,
            exposed: exp,
            rank: (4, i),
            unscopable,
            conditional,
        });
    }
    if d.stringifier && !d.cands.iter().any(|c| c.name == "toString") {
        let place = if is_global {
            Place::Instance
        } else {
            Place::Proto
        };
        d.cands.push(Cand {
            name: "toString".into(),
            place,
            kind: CandKind::Op(op("toString", 0, OpKind::Stringifier)),
            features: Vec::new(),
            exposed: iface_exposed,
            rank: (5, 0),
            unscopable: false,
            conditional: false,
        });
    }

    // Members generated by iterable / maplike / setlike declarations.
    let synth = |name: &str, len: usize, kind: OpKind, group: u8, i: usize| Cand {
        name: name.into(),
        place: Place::Proto,
        kind: CandKind::Op(op(name, len, kind)),
        features: Vec::new(),
        exposed: iface_exposed,
        rank: (group, i),
        unscopable: false,
        conditional: false,
    };
    match d.iterable {
        Iterable::Value if d.indexed_getter => {
            for (i, (n, a, len)) in [
                ("entries", "Entries", 0),
                ("keys", "Keys", 0),
                ("values", "Values", 0),
                ("forEach", "ForEach", 1),
            ]
            .into_iter()
            .enumerate()
            {
                d.cands.push(synth(n, len, OpKind::Array(a), 0, i));
            }
        }
        Iterable::Value | Iterable::Pair => {
            for (n, len) in [("entries", 0), ("forEach", 1), ("keys", 0), ("values", 0)] {
                d.cands.push(synth(n, len, OpKind::Iteration, 6, 0));
            }
        }
        Iterable::AsyncValue => d.cands.push(synth("values", 0, OpKind::Iteration, 6, 0)),
        Iterable::AsyncPair => {
            for n in ["entries", "keys", "values"] {
                d.cands.push(synth(n, 0, OpKind::Iteration, 6, 0));
            }
        }
        Iterable::Maplike { readonly } => {
            let mut names = vec![
                ("entries", 0),
                ("forEach", 1),
                ("get", 1),
                ("has", 1),
                ("keys", 0),
                ("values", 0),
            ];
            if !readonly {
                names.extend([("clear", 0), ("delete", 1), ("set", 2)]);
            }
            for (n, len) in names {
                d.cands.push(synth(n, len, OpKind::Iteration, 6, 0));
            }
        }
        Iterable::Setlike { readonly } => {
            let mut names = vec![
                ("entries", 0),
                ("forEach", 1),
                ("has", 1),
                ("keys", 0),
                ("values", 0),
            ];
            if !readonly {
                names.extend([("add", 1), ("clear", 0), ("delete", 1)]);
            }
            for (n, len) in names {
                d.cands.push(synth(n, len, OpKind::Iteration, 6, 0));
            }
        }
        Iterable::None => {}
    }
    if matches!(
        d.iterable,
        Iterable::Maplike { .. } | Iterable::Setlike { .. }
    ) && !d.cands.iter().any(|c| c.name == "size")
    {
        d.cands.push(Cand {
            name: "size".into(),
            place: Place::Proto,
            kind: CandKind::Attr(Attr {
                name: "size".into(),
                readonly: true,
                is_static: false,
                unforgeable: false,
                replaceable: false,
                same_object: false,
                put_forwards: None,
                on_instance: false,
                enumerable: true,
                late: false,
                ty: AttrTy::Number,
                secure_only: false,
            }),
            features: Vec::new(),
            exposed: iface_exposed,
            rank: (2, 0),
            unscopable: false,
            conditional: false,
        });
    }
    d
}

/// IDL-only order guess for one place: Blink's generator puts attributes
/// first, then constants, then operations sorted by name, then iteration
/// methods sorted by name; indexed iterables start with the four
/// `Array.prototype` functions.
fn natural_order(cands: &[&Cand]) -> Vec<String> {
    let mut v: Vec<&&Cand> = cands.iter().collect();
    v.sort_by(|a, b| {
        let ka = (
            a.conditional,
            a.rank.0,
            if a.rank.0 >= 4 { 0 } else { a.rank.1 },
        );
        let kb = (
            b.conditional,
            b.rank.0,
            if b.rank.0 >= 4 { 0 } else { b.rank.1 },
        );
        ka.cmp(&kb).then_with(|| {
            if a.rank.0 >= 4 {
                a.name.cmp(&b.name)
            } else {
                std::cmp::Ordering::Equal
            }
        })
    });
    v.into_iter().map(|c| c.name.clone()).collect()
}

/// Fill one member list from fixture properties, matching IDL candidates.
struct Builder<'a> {
    out: &'a mut Interface,
}

impl Builder<'_> {
    fn push_const(&mut self, name: &str, v: ConstV) -> Slot {
        if let Some(i) = self.out.constants.iter().position(|(n, _)| n == name) {
            return Slot::Const(i);
        }
        self.out.constants.push((name.to_owned(), v));
        Slot::Const(self.out.constants.len() - 1)
    }
    fn push_attr(&mut self, a: Attr) -> Slot {
        self.out.attributes.push(a);
        Slot::Attr(self.out.attributes.len() - 1)
    }
    fn push_op(&mut self, o: Op) -> Slot {
        self.out.operations.push(o);
        Slot::Op(self.out.operations.len() - 1)
    }
}

fn attr_from_fixture(p: &FixProp, place: Place) -> Attr {
    Attr {
        name: p.name.clone(),
        readonly: !p.flag('s'),
        is_static: place == Place::Static,
        unforgeable: !p.flag('c'),
        replaceable: false,
        same_object: false,
        put_forwards: None,
        on_instance: place == Place::Instance,
        enumerable: p.flag('e'),
        late: false,
        ty: AttrTy::Other,
        secure_only: false,
    }
}

/// Match fixture properties against candidates; returns the slots in
/// fixture order.
#[allow(clippy::too_many_arguments)]
fn match_props(
    iface_name: &str,
    props: &[FixProp],
    place: Place,
    late_from: usize,
    cands: &[Cand],
    b: &mut Builder<'_>,
    cov: &mut Coverage,
    used: &mut HashSet<(String, u8)>,
    exact: &mut bool,
) -> Vec<Slot> {
    let mut slots = Vec::new();
    for (i, p) in props.iter().enumerate() {
        let late = i >= late_from;
        let cand = cands
            .iter()
            .find(|c| c.place == place && c.name == p.name && c.kind.code() == p.kind());
        cov.fixture_members += 1;
        let slot = match cand {
            Some(c) => {
                cov.idl_members += 1;
                used.insert((c.name.clone(), place as u8));
                match &c.kind {
                    CandKind::Const(v) => b.push_const(&c.name, *v),
                    CandKind::Attr(a) => {
                        let mut a = a.clone();
                        let fix_setter = p.flag('s');
                        if a.has_setter() != fix_setter {
                            cov.warnings.push(format!(
                                "{iface_name}.{}: IDL setter {} vs Chrome {fix_setter}",
                                a.name,
                                a.has_setter()
                            ));
                            // The fixture wins; keep IDL flags when they agree.
                            a.readonly = !fix_setter;
                            if !fix_setter {
                                a.replaceable = false;
                                a.put_forwards = None;
                            }
                            *exact = false;
                        }
                        a.enumerable = p.flag('e');
                        a.unforgeable = !p.flag('c');
                        a.on_instance = place == Place::Instance;
                        a.late = late;
                        b.push_attr(a)
                    }
                    CandKind::Op(o) => {
                        let mut o = o.clone();
                        if let Some(len) = p.len
                            && len != o.length
                        {
                            cov.length_mismatch.push(format!(
                                "{iface_name}.{} idl={} chrome={len}",
                                o.name, o.length
                            ));
                            o.length = len;
                            *exact = false;
                        }
                        o.enumerable = p.flag('e');
                        o.unforgeable = !p.flag('c');
                        o.on_instance = place == Place::Instance;
                        o.late = late;
                        b.push_op(o)
                    }
                }
            }
            None => {
                *exact = false;
                cov.fixture_only
                    .push(format!("{iface_name}.{} ({})", p.name, p.code));
                match p.kind() {
                    'v' => b.push_const(&p.name, ConstV::Number(0.0)),
                    'g' => {
                        let mut a = attr_from_fixture(p, place);
                        a.late = late;
                        b.push_attr(a)
                    }
                    _ => {
                        let mut o = op(&p.name, p.len.unwrap_or(0) as usize, OpKind::Regular);
                        o.is_static = place == Place::Static;
                        o.on_instance = place == Place::Instance;
                        o.enumerable = p.flag('e');
                        o.unforgeable = !p.flag('c');
                        o.late = late;
                        b.push_op(o)
                    }
                }
            }
        };
        slots.push(slot);
    }
    slots
}

fn empty_interface(name: &str) -> Interface {
    Interface {
        name: name.to_owned(),
        parent: None,
        exposed: exposed::WINDOW,
        is_global: false,
        legacy_window_alias: Vec::new(),
        has_constructor: false,
        constructor_length: 0,
        legacy_factory: None,
        constants: Vec::new(),
        attributes: Vec::new(),
        operations: Vec::new(),
        stringifier: false,
        stringifier_attr: None,
        iterable: Iterable::None,
        indexed_getter: false,
        named_getter: false,
        indexed_setter: false,
        named_setter: false,
        named_deleter: false,
        legacy_unforgeable: false,
        to_string_tag: String::new(),
        proto_order: Vec::new(),
        static_order: Vec::new(),
        instance_order: Vec::new(),
        iterator: SymIter::None,
        async_iterator: None,
        unscopables: Vec::new(),
        callback_interface: false,
        from_fixture: false,
        builtin_parent: None,
        secure_only: false,
    }
}

fn base_interface(iface: &IdlInterface, d: &Derived) -> Interface {
    let mut out = empty_interface(&iface.name);
    out.parent = iface.parent.clone();
    out.exposed = {
        let e = exposure_bits(&iface.ext.idents("Exposed"));
        if e == 0 { exposed::WINDOW } else { e }
    };
    out.is_global = iface.ext.has("Global");
    out.has_constructor = d.has_constructor;
    out.constructor_length = d.constructor_length;
    if let Some(ExtValue::NamedArgs(n, args)) = iface.ext.get("LegacyFactoryFunction") {
        out.legacy_factory = Some((n.clone(), required_args(args) as u8));
    }
    out.stringifier = d.stringifier;
    out.stringifier_attr = d.stringifier_attr.clone();
    out.iterable = d.iterable;
    out.indexed_getter = d.indexed_getter;
    out.named_getter = d.named_getter;
    out.indexed_setter = d.indexed_setter;
    out.named_setter = d.named_setter;
    out.named_deleter = d.named_deleter;
    out.legacy_unforgeable = iface.ext.has("LegacyUnforgeable");
    out.callback_interface = iface.kind == DefKind::CallbackInterface;
    out
}

fn symbol_iterator(d: &Derived) -> SymIter {
    match d.iterable {
        Iterable::Pair | Iterable::Maplike { .. } => SymIter::Alias("entries".into()),
        Iterable::Setlike { .. } => SymIter::Alias("values".into()),
        _ => SymIter::ArrayValues,
    }
}

/// Features with evidence: a gated member or interface present in the
/// fixture means the feature is on.
fn enabled_features(
    model: &Model,
    fix: &Fixture,
    derived: &BTreeMap<String, Derived>,
    window_names: &HashSet<&str>,
) -> HashSet<String> {
    let mut on = HashSet::new();
    for (name, iface) in &model.interfaces {
        if window_names.contains(name.as_str()) {
            on.extend(iface.features());
        }
        let (Some(f), Some(d)) = (fix.interfaces.get(name), derived.get(name)) else {
            continue;
        };
        for c in &d.cands {
            let list = match c.place {
                Place::Proto => &f.proto,
                Place::Static => &f.statics,
                Place::Instance => continue,
            };
            if list.iter().any(|p| p.name == c.name) {
                on.extend(c.features.iter().cloned());
            }
        }
    }
    // Window members are checked against the window's own properties.
    if let Some(d) = derived.get("Window") {
        for c in &d.cands {
            if c.place == Place::Instance && fix.window.iter().any(|p| p.name == c.name) {
                on.extend(c.features.iter().cloned());
            }
        }
    }
    on
}

/// Build the Chrome tables from the model and the fixture JSON (the
/// arbiter, captured in a secure context). With `insecure`, a capture of a
/// non-secure page (`about:blank`), everything it lacks is marked secure
/// only.
pub fn build(
    model: &Model,
    fixture: &Value,
    insecure: Option<&Value>,
    workers: Option<&Value>,
) -> Result<Table, String> {
    let fix = load_fixture(fixture)?;
    let insecure = insecure.map(load_fixture).transpose()?;
    let workers = workers
        .map(load_worker_fixture)
        .transpose()?
        .unwrap_or_default();
    let mut cov = Coverage {
        warnings: model.warnings.clone(),
        ..Coverage::default()
    };

    let derived: BTreeMap<String, Derived> = model
        .interfaces
        .iter()
        .map(|(n, i)| (n.clone(), derive(model, i)))
        .collect();

    // Aliases and legacy factories.
    let mut alias_of: HashMap<String, String> = HashMap::new();
    let mut factory_of: HashMap<String, String> = HashMap::new();
    for (name, iface) in &model.interfaces {
        for a in iface.ext.idents("LegacyWindowAlias") {
            alias_of.insert(a, name.clone());
        }
        if let Some(ExtValue::NamedArgs(n, _)) = iface.ext.get("LegacyFactoryFunction") {
            factory_of.insert(n.clone(), name.clone());
        }
    }
    let is_interface = |n: &str| {
        model
            .interfaces
            .get(n)
            .is_some_and(|i| matches!(i.kind, DefKind::Interface | DefKind::CallbackInterface))
    };

    let window_names: HashSet<&str> = fix.window.iter().map(|p| p.name.as_str()).collect();
    let on = enabled_features(model, &fix, &derived, &window_names);
    let enabled = |c: &Cand| c.features.iter().all(|f| on.contains(f));

    // Global props of one scope, classified: `props` are the global's own
    // properties, `ifaces` the scope's captured interfaces, `global` the
    // global interface's IDL candidates.
    let classify = |props: &[FixProp], ifaces: &HashMap<String, FixIface>, global: &Derived| {
        let mut out = Vec::new();
        for p in props {
            let n = p.name.clone();
            let fix_alias = ifaces
                .get(&n)
                .and_then(|f| f.ctor_to_string.as_ref())
                .and_then(|s| {
                    s.strip_prefix("function ")
                        .and_then(|s| s.split('(').next())
                        .map(str::to_owned)
                });
            let prop =
                if let Some(t) = alias_of.get(&n) {
                    GlobalProp::Alias {
                        name: n,
                        target: t.clone(),
                    }
                } else if let Some(t) = fix_alias.filter(|t| *t != n && is_interface(t)) {
                    GlobalProp::Alias { name: n, target: t }
                } else if is_interface(&n) {
                    GlobalProp::Interface(n)
                } else if let Some(t) = factory_of.get(&n) {
                    GlobalProp::LegacyFactory {
                        name: n,
                        iface: t.clone(),
                    }
                } else if model
                    .interfaces
                    .get(&n)
                    .is_some_and(|i| i.kind == DefKind::Namespace)
                {
                    GlobalProp::Namespace(n)
                } else if let Some(c) = global.cands.iter().find(|c| {
                    c.place == Place::Instance && c.name == n && c.kind.code() == p.kind()
                }) {
                    match c.kind {
                        CandKind::Attr(_) => GlobalProp::Attr(n),
                        _ => GlobalProp::Op(n),
                    }
                } else if V8_GLOBALS.contains(&n.as_str()) || ifaces.contains_key(&n) {
                    GlobalProp::Builtin(n)
                } else {
                    GlobalProp::Other(n)
                };
            out.push(prop);
        }
        out
    };
    let window_derived = derived.get("Window").ok_or("IDL has no Window")?;
    let window_props = classify(&fix.window, &fix.interfaces, window_derived);
    cov.window_props = window_props.len();
    cov.window_props_resolved = window_props
        .iter()
        .filter(|p| !matches!(p, GlobalProp::Other(_)))
        .count();
    let window_late_start = late_start(&window_props);
    // The same for each worker scope.
    let mut worker_props: Vec<Vec<GlobalProp>> = Vec::new();
    for w in &workers {
        let d = derived
            .get(&w.global)
            .ok_or_else(|| format!("IDL has no {}", w.global))?;
        worker_props.push(classify(&w.props, &w.interfaces, d));
    }

    // Interfaces in window order, then worker-only ones.
    let mut names: Vec<String> = Vec::new();
    let mut seen = HashSet::new();
    for p in &window_props {
        let n = match p {
            GlobalProp::Interface(n) => n,
            GlobalProp::Alias { target, .. } => target,
            GlobalProp::LegacyFactory { iface, .. } => iface,
            _ => continue,
        };
        if seen.insert(n.clone()) {
            names.push(n.clone());
        }
    }
    let window_iface_count = names.len();
    // Worker-only interfaces, in name order; with a worker fixture, every
    // interface of a worker global that the window lacks, described by
    // the first scope that has it.
    let mut worker_only: Vec<String> = if workers.is_empty() {
        fix.worker
            .iter()
            .filter(|n| is_interface(n) && !seen.contains(*n))
            .cloned()
            .collect()
    } else {
        workers
            .iter()
            .zip(&worker_props)
            .flat_map(|(_, props)| props.iter())
            .filter_map(|p| match p {
                GlobalProp::Interface(n) => Some(n.clone()),
                GlobalProp::Alias { target, .. } => Some(target.clone()),
                _ => None,
            })
            .filter(|n| !seen.contains(n))
            .collect()
    };
    worker_only.sort();
    worker_only.dedup();
    for n in worker_only {
        seen.insert(n.clone());
        names.push(n);
    }
    let worker_fixture = |name: &str| workers.iter().find_map(|w| w.interfaces.get(name));

    let mut out_ifaces: Vec<Interface> = Vec::new();
    for (pos, name) in names.iter().enumerate() {
        let iface = &model.interfaces[name];
        let d = &derived[name];
        let mut out = base_interface(iface, d);
        let in_window = pos < window_iface_count;
        let fixture = if in_window {
            fix.interfaces.get(name)
        } else {
            worker_fixture(name)
        };
        if in_window {
            cov.fixture_interfaces += 1;
            cov.idl_interfaces += 1;
        }
        let mut exact = true;
        let mut order_ok = true;
        let mut used = HashSet::new();
        if let Some(f) = fixture {
            out.from_fixture = true;
            out.constructor_length = f.length;
            out.parent = f.parent.clone().filter(|p| p != "Object");
            if out.parent.is_some()
                && iface.parent.is_none()
                && !is_interface(out.parent.as_deref().unwrap_or(""))
            {
                out.builtin_parent = out.parent.take();
            } else if out.parent != iface.parent {
                cov.warnings.push(format!(
                    "{name}: fixture parent {:?} vs IDL {:?}",
                    out.parent, iface.parent
                ));
                out.parent = iface.parent.clone();
            }
            out.to_string_tag = f.to_string_tag.clone().unwrap_or_default();
            let ctor_at = f
                .proto
                .iter()
                .position(|p| p.name == "constructor")
                .unwrap_or(f.proto.len());
            let proto: Vec<FixProp> = f
                .proto
                .iter()
                .filter(|p| p.name != "constructor")
                .cloned()
                .collect();
            let mut b = Builder { out: &mut out };
            let proto_slots = match_props(
                name,
                &proto,
                Place::Proto,
                ctor_at,
                &d.cands,
                &mut b,
                &mut cov,
                &mut used,
                &mut exact,
            );
            let static_slots = match_props(
                name,
                &f.statics,
                Place::Static,
                usize::MAX,
                &d.cands,
                &mut b,
                &mut cov,
                &mut used,
                &mut exact,
            );
            out.proto_order = proto_slots;
            out.static_order = static_slots;

            // IDL-only order check.
            for (place, list) in [(Place::Proto, &proto), (Place::Static, &f.statics)] {
                let fixture_set: HashSet<&str> = list.iter().map(|p| p.name.as_str()).collect();
                let cands: Vec<&Cand> = d
                    .cands
                    .iter()
                    .filter(|c| c.place == place && fixture_set.contains(c.name.as_str()))
                    .collect();
                let nat = natural_order(&cands);
                let fix_order: Vec<String> = list.iter().map(|p| p.name.clone()).collect();
                if nat != fix_order {
                    order_ok = false;
                }
            }

            let has = |s: &str| f.symbols.iter().any(|x| x == s);
            if has("Symbol(Symbol.iterator)") {
                out.iterator = symbol_iterator(d);
            }
            if has("Symbol(Symbol.asyncIterator)") {
                out.async_iterator = Some(match d.iterable {
                    Iterable::AsyncPair => "entries".into(),
                    _ => "values".into(),
                });
            }
            if has("Symbol(Symbol.unscopables)") {
                let mut u: Vec<String> = d
                    .cands
                    .iter()
                    .filter(|c| c.unscopable && c.place == Place::Proto && enabled(c))
                    .map(|c| c.name.clone())
                    .collect();
                u.dedup();
                out.unscopables = u;
            }
            if out.is_global {
                // Members of the global come from its own props (the
                // window's, or a worker scope's).
                let (gprops, classified, late_from): (&[FixProp], &[GlobalProp], usize) =
                    match workers.iter().position(|w| &w.global == name) {
                        Some(k) => (
                            &workers[k].props,
                            &worker_props[k],
                            late_start(&worker_props[k]),
                        ),
                        None => (&fix.window, &window_props, window_late_start),
                    };
                let mut b = Builder { out: &mut out };
                let mut slots = Vec::new();
                for (i, (p, gp)) in gprops.iter().zip(classified).enumerate() {
                    if !matches!(gp, GlobalProp::Attr(_) | GlobalProp::Op(_)) {
                        continue;
                    }
                    let slot = match_props(
                        name,
                        std::slice::from_ref(p),
                        Place::Instance,
                        if i >= late_from { 0 } else { 1 },
                        &d.cands,
                        &mut b,
                        &mut cov,
                        &mut used,
                        &mut exact,
                    );
                    slots.extend(slot);
                }
                out.instance_order = slots;
            }
            if exact {
                cov.exact_members += 1;
                if order_ok {
                    cov.exact_order += 1;
                }
            }
            if !order_ok {
                cov.order_mismatch.push(name.clone());
            }
        } else {
            // Not in the fixture: IDL order, enabled features only.
            out.to_string_tag = name.clone();
            let mut cands: Vec<&Cand> = d
                .cands
                .iter()
                .filter(|c| c.place != Place::Instance && enabled(c))
                .filter(|c| in_window || c.exposed & exposed::WORKER != 0)
                .collect();
            cands.sort_by_key(|c| c.place as u8);
            for place in [Place::Proto, Place::Static] {
                let these: Vec<&Cand> =
                    cands.iter().copied().filter(|c| c.place == place).collect();
                let order = natural_order(&these);
                let mut b = Builder { out: &mut out };
                let mut slots = Vec::new();
                for n in order {
                    let c = these.iter().find(|c| c.name == n).expect("candidate");
                    slots.push(match &c.kind {
                        CandKind::Const(v) => b.push_const(&c.name, *v),
                        CandKind::Attr(a) => b.push_attr(a.clone()),
                        CandKind::Op(o) => b.push_op(o.clone()),
                    });
                }
                match place {
                    Place::Proto => out.proto_order = slots,
                    _ => out.static_order = slots,
                }
            }
            if matches!(
                d.iterable,
                Iterable::Pair | Iterable::Maplike { .. } | Iterable::Setlike { .. }
            ) || d.indexed_getter
            {
                out.iterator = symbol_iterator(d);
            }
            if matches!(d.iterable, Iterable::AsyncValue | Iterable::AsyncPair) {
                out.async_iterator = Some(if d.iterable == Iterable::AsyncPair {
                    "entries".into()
                } else {
                    "values".into()
                });
            }
        }

        // Unforgeable instance members of non-global interfaces: not in the
        // fixture (it lists prototypes), so filter by features and exposure.
        if !out.is_global {
            let want = if in_window {
                exposed::WINDOW
            } else {
                exposed::WORKER
            };
            let mut b = Builder { out: &mut out };
            let mut slots = Vec::new();
            for c in d
                .cands
                .iter()
                .filter(|c| c.place == Place::Instance && enabled(c) && c.exposed & want != 0)
            {
                slots.push(match &c.kind {
                    CandKind::Const(v) => b.push_const(&c.name, *v),
                    CandKind::Attr(a) => b.push_attr(a.clone()),
                    CandKind::Op(o) => b.push_op(o.clone()),
                });
            }
            out.instance_order = slots;
        }

        if in_window {
            out.legacy_window_alias = iface
                .ext
                .idents("LegacyWindowAlias")
                .into_iter()
                .filter(|a| window_names.contains(a.as_str()))
                .collect();
            if let Some((n, _)) = &out.legacy_factory {
                match fix.window.iter().find(|p| &p.name == n) {
                    Some(p) => out.legacy_factory = Some((n.clone(), p.len.unwrap_or(0))),
                    None => out.legacy_factory = None,
                }
            }
            out.exposed |= exposed::WINDOW;
        } else {
            out.legacy_window_alias.clear();
            out.legacy_factory = None;
        }
        out_ifaces.push(out);
    }

    // Parents must exist and come first.
    let present: HashSet<String> = out_ifaces.iter().map(|i| i.name.clone()).collect();
    let mut missing_parents: BTreeSet<String> = BTreeSet::new();
    for i in &out_ifaces {
        if let Some(p) = &i.parent
            && !present.contains(p)
        {
            missing_parents.insert(p.clone());
        }
    }
    for p in missing_parents {
        cov.warnings
            .push(format!("parent {p} not exposed; children lose it"));
        for i in out_ifaces.iter_mut() {
            if i.parent.as_deref() == Some(&p) {
                i.parent = model
                    .ancestors(&i.name)
                    .iter()
                    .map(|a| a.name.clone())
                    .find(|a| present.contains(a));
            }
        }
    }
    // Worker scopes: their global props and order overrides.
    let mut worker_scopes = Vec::new();
    for (w, props) in workers.iter().zip(&worker_props) {
        let overrides = worker_overrides(w, props, &mut out_ifaces, &derived);
        worker_scopes.push(WorkerScope {
            global: w.global.clone(),
            exposed: w.exposed,
            props: props.clone(),
            late_start: late_start(props),
            overrides,
        });
    }
    let ordered = topo_sort(out_ifaces);

    // Namespaces on the window, then those only workers have.
    let mut namespaces: Vec<Namespace> = Vec::new();
    for p in window_props.iter().chain(worker_props.iter().flatten()) {
        let GlobalProp::Namespace(n) = p else {
            continue;
        };
        if namespaces.iter().any(|x| &x.name == n) {
            continue;
        }
        let iface = &model.interfaces[n];
        let d = &derived[n];
        let mut tmp = empty_interface(n);
        let cands: Vec<&Cand> = d
            .cands
            .iter()
            .filter(|c| c.place == Place::Static && enabled(c))
            .filter(|c| c.exposed & (exposed::WINDOW | exposed::WORKER) != 0)
            .collect();
        let mut b = Builder { out: &mut tmp };
        let mut order = Vec::new();
        for name in natural_order(&cands) {
            let c = cands.iter().find(|c| c.name == name).expect("candidate");
            order.push(match &c.kind {
                CandKind::Const(v) => b.push_const(&c.name, *v),
                CandKind::Attr(a) => b.push_attr(a.clone()),
                CandKind::Op(o) => b.push_op(o.clone()),
            });
        }
        namespaces.push(Namespace {
            name: n.clone(),
            exposed: {
                let e = exposure_bits(&iface.ext.idents("Exposed"));
                if e == 0 { exposed::WINDOW } else { e }
            },
            constants: tmp.constants,
            attributes: tmp.attributes,
            operations: tmp.operations,
            order,
            to_string_tag: n.clone(),
        });
    }

    let mut ordered = ordered;
    let mut window_secure_only = Vec::new();
    if let Some(ins) = &insecure {
        mark_secure(&mut ordered, ins);
        let names: HashSet<&str> = ins.window.iter().map(|p| p.name.as_str()).collect();
        window_secure_only = window_props
            .iter()
            .map(GlobalProp::name)
            .filter(|n| !names.contains(n))
            .map(str::to_owned)
            .collect();
    }

    Ok(Table {
        interfaces: ordered,
        namespaces,
        window_props,
        window_late_start,
        window_secure_only,
        os_gated: Vec::new(),
        workers: worker_scopes,
        coverage: cov,
    })
}

/// Merge the OS overlay (`tests/fixtures/chrome_153_os_overlay.json`) into
/// a fixture: members Chrome 153 has on Windows and macOS but the Linux
/// capture lacks. `secure` is true for the secure-context fixture; the
/// non-secure one only gets entries marked `"secure": false`. Each entry is
/// inserted after the property its `after` names, in the order given.
pub fn apply_overlay(fixture: &mut Value, overlay: &Value, secure: bool) -> Result<(), String> {
    let wanted = |e: &Value| secure || e["secure"].as_bool() == Some(false);
    let insert = |list: &mut Vec<Value>, e: &Value, what: &str| -> Result<(), String> {
        let after = e["after"].as_str().ok_or("overlay: entry without after")?;
        let at = list
            .iter()
            .position(|p| p.get(0).and_then(Value::as_str) == Some(after))
            .ok_or_else(|| format!("overlay: {what} has no {after}"))?;
        let props = e["props"]
            .as_array()
            .ok_or("overlay: entry without props")?;
        for (k, p) in props.iter().enumerate() {
            let name = p.get(0).and_then(Value::as_str).unwrap_or_default();
            if list
                .iter()
                .any(|q| q.get(0).and_then(Value::as_str) == Some(name))
            {
                return Err(format!("overlay: {what} already has {name}"));
            }
            list.insert(at + 1 + k, p.clone());
        }
        Ok(())
    };
    for e in overlay["members"].as_array().into_iter().flatten() {
        if !wanted(e) {
            continue;
        }
        let iface = e["interface"]
            .as_str()
            .ok_or("overlay: member without interface")?;
        let list = fixture["interfaces"][iface]["proto"]
            .as_array_mut()
            .ok_or_else(|| format!("overlay: fixture has no {iface}"))?;
        insert(list, e, iface)?;
    }
    for e in overlay["windowProps"].as_array().into_iter().flatten() {
        if !wanted(e) {
            continue;
        }
        let list = fixture["windowProps"]
            .as_array_mut()
            .ok_or("overlay: fixture has no windowProps")?;
        insert(list, e, "windowProps")?;
    }
    for (name, f) in overlay["interfaces"].as_object().into_iter().flatten() {
        if !wanted(f) {
            continue;
        }
        let map = fixture["interfaces"]
            .as_object_mut()
            .ok_or("overlay: fixture has no interfaces")?;
        if map.contains_key(name) {
            return Err(format!("overlay: fixture already has interface {name}"));
        }
        let mut f = f.clone();
        if let Some(o) = f.as_object_mut() {
            o.remove("secure");
        }
        map.insert(name.clone(), f);
    }
    Ok(())
}

/// The gated members of the overlay, `(gate, owner, name)`, checked
/// against the built table: every interface the overlay adds (as a window
/// property), its prototype members and statics, and each member entry.
pub fn overlay_gates(
    overlay: &Value,
    table: &Table,
) -> Result<Vec<(String, String, String)>, String> {
    let mut out = Vec::new();
    for e in overlay["members"].as_array().into_iter().flatten() {
        let gate = e["gate"].as_str().ok_or("overlay: member without gate")?;
        let iface = e["interface"].as_str().unwrap_or_default();
        let i = table
            .interface(iface)
            .ok_or_else(|| format!("overlay: table has no {iface}"))?;
        for p in e["props"].as_array().into_iter().flatten() {
            let name = p.get(0).and_then(Value::as_str).unwrap_or_default();
            let found = i
                .proto_order
                .iter()
                .chain(&i.static_order)
                .any(|s| i.slot_name(*s) == name);
            if !found {
                return Err(format!("overlay: {iface}.{name} did not reach the table"));
            }
            out.push((gate.to_owned(), iface.to_owned(), name.to_owned()));
        }
    }
    for e in overlay["windowProps"].as_array().into_iter().flatten() {
        let gate = e["gate"]
            .as_str()
            .ok_or("overlay: window prop without gate")?;
        for p in e["props"].as_array().into_iter().flatten() {
            let name = p.get(0).and_then(Value::as_str).unwrap_or_default();
            if !table.window_props.iter().any(|g| g.name() == name) {
                return Err(format!(
                    "overlay: window prop {name} did not reach the table"
                ));
            }
            out.push((gate.to_owned(), "Window".to_owned(), name.to_owned()));
        }
    }
    Ok(out)
}

/// First property Chrome installs after the engine's late built-ins: the
/// first engine global (or unknown property) after the first Blink one.
fn late_start(props: &[GlobalProp]) -> usize {
    let first_blink = props
        .iter()
        .position(|p| !matches!(p, GlobalProp::Builtin(_)))
        .unwrap_or(0);
    props
        .iter()
        .skip(first_blink)
        .position(|p| matches!(p, GlobalProp::Builtin(_) | GlobalProp::Other(_)))
        .map_or(props.len(), |i| i + first_blink)
}

/// The slot of `iface`'s member `p` at `place`, reusing the interface's
/// member of that name and kind when it has one, else adding it (from the
/// IDL candidate, or from the fixture alone).
fn worker_slot(iface: &mut Interface, p: &FixProp, place: Place, cands: &[Cand]) -> Slot {
    let is_static = place == Place::Static;
    match p.kind() {
        'v' => {
            if let Some(i) = iface.constants.iter().position(|(n, _)| *n == p.name) {
                return Slot::Const(i);
            }
        }
        'g' => {
            if let Some(i) = iface
                .attributes
                .iter()
                .position(|a| a.name == p.name && a.is_static == is_static && !a.on_instance)
            {
                return Slot::Attr(i);
            }
        }
        _ => {
            if let Some(i) = iface
                .operations
                .iter()
                .position(|o| o.name == p.name && o.is_static == is_static && !o.on_instance)
            {
                return Slot::Op(i);
            }
        }
    }
    let mut cov = Coverage::default();
    let (mut used, mut exact) = (HashSet::new(), true);
    let mut b = Builder { out: iface };
    let name = b.out.name.clone();
    match_props(
        &name,
        std::slice::from_ref(p),
        place,
        usize::MAX,
        cands,
        &mut b,
        &mut cov,
        &mut used,
        &mut exact,
    )[0]
}

/// Interface order overrides of one worker scope: every interface of the
/// scope whose captured prototype or statics (names, order, or members
/// after `constructor`) differ from its table entry.
fn worker_overrides(
    w: &WorkerFix,
    props: &[GlobalProp],
    ifaces: &mut [Interface],
    derived: &BTreeMap<String, Derived>,
) -> Vec<IfaceOrder> {
    let mut names: Vec<String> = Vec::new();
    for p in props {
        let n = match p {
            GlobalProp::Interface(n) => n,
            GlobalProp::Alias { target, .. } => target,
            _ => continue,
        };
        if !names.contains(n) {
            names.push(n.clone());
        }
    }
    let mut out = Vec::new();
    for name in names {
        let (Some(f), Some(d)) = (w.interfaces.get(&name), derived.get(&name)) else {
            continue;
        };
        let Some(iface) = ifaces.iter_mut().find(|i| i.name == name) else {
            continue;
        };
        let ctor_at = f
            .proto
            .iter()
            .position(|p| p.name == "constructor")
            .unwrap_or(f.proto.len());
        let proto: Vec<&FixProp> = f.proto.iter().filter(|p| p.name != "constructor").collect();
        // The table entry's view: names in order, late members after the
        // non-late ones (where `constructor` goes).
        let late = |i: &Interface, s: &Slot| match *s {
            Slot::Attr(x) => i.attributes[x].late,
            Slot::Op(x) => i.operations[x].late,
            Slot::Const(_) => false,
        };
        let mut have: Vec<(String, bool)> = iface
            .proto_order
            .iter()
            .map(|s| (iface.slot_name(*s).to_owned(), late(iface, s)))
            .collect();
        have.sort_by_key(|(_, l)| *l);
        let want: Vec<(String, bool)> = proto
            .iter()
            .enumerate()
            .map(|(i, p)| (p.name.clone(), i >= ctor_at))
            .collect();
        let have_static: Vec<&str> = iface
            .static_order
            .iter()
            .map(|s| iface.slot_name(*s))
            .collect();
        let want_static: Vec<&str> = f.statics.iter().map(|p| p.name.as_str()).collect();
        if have == want && have_static == want_static {
            continue;
        }
        let proto_order = proto
            .iter()
            .map(|p| worker_slot(iface, p, Place::Proto, &d.cands))
            .collect();
        let static_order = f
            .statics
            .iter()
            .map(|p| worker_slot(iface, p, Place::Static, &d.cands))
            .collect();
        out.push(IfaceOrder {
            iface: name,
            proto_order,
            ctor_at,
            static_order,
        });
    }
    out
}

/// Flag what a non-secure capture lacks.
fn mark_secure(list: &mut [Interface], ins: &Fixture) {
    let window: HashSet<&str> = ins.window.iter().map(|p| p.name.as_str()).collect();
    for i in list.iter_mut().filter(|i| i.from_fixture) {
        let Some(f) = ins.interfaces.get(&i.name) else {
            // Callback interfaces (NodeFilter) are not in the map.
            i.secure_only = !window.contains(i.name.as_str());
            continue;
        };
        let lists: [(&Vec<Slot>, &Vec<FixProp>); 2] =
            [(&i.proto_order, &f.proto), (&i.static_order, &f.statics)];
        let mut flags = Vec::new();
        for (slots, props) in lists {
            for s in slots {
                let name = i.slot_name(*s);
                if !props.iter().any(|p| p.name == name) {
                    flags.push(*s);
                }
            }
        }
        if i.is_global {
            for s in &i.instance_order {
                if !window.contains(i.slot_name(*s)) {
                    flags.push(*s);
                }
            }
        }
        for s in flags {
            match s {
                Slot::Attr(x) => i.attributes[x].secure_only = true,
                Slot::Op(x) => i.operations[x].secure_only = true,
                Slot::Const(_) => {}
            }
        }
    }
}

/// Stable order with every parent before its children.
fn topo_sort(list: Vec<Interface>) -> Vec<Interface> {
    let mut done: HashSet<String> = HashSet::new();
    let mut out = Vec::with_capacity(list.len());
    let by_name: HashMap<String, usize> = list
        .iter()
        .enumerate()
        .map(|(i, x)| (x.name.clone(), i))
        .collect();
    fn visit(
        i: usize,
        list: &[Interface],
        by_name: &HashMap<String, usize>,
        done: &mut HashSet<String>,
        out: &mut Vec<usize>,
    ) {
        if done.contains(&list[i].name) {
            return;
        }
        if let Some(p) = &list[i].parent
            && let Some(&pi) = by_name.get(p)
        {
            visit(pi, list, by_name, done, out);
        }
        done.insert(list[i].name.clone());
        out.push(i);
    }
    let mut idx = Vec::new();
    for i in 0..list.len() {
        visit(i, &list, &by_name, &mut done, &mut idx);
    }
    let mut slots: Vec<Option<Interface>> = list.into_iter().map(Some).collect();
    for i in idx {
        out.push(slots[i].take().expect("visited once"));
    }
    out
}

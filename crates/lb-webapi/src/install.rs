//! The runtime installer: generated descriptor tables + a [`Registry`] of
//! implementations → `lb_js` interfaces, and window realms built from them.
//!
//! WebIDL rules applied here (Chrome's behaviour):
//! - Operations: `writable`, `configurable`, `enumerable` unless Blink marks
//!   them otherwise; attributes are accessors, `configurable` unless
//!   `[LegacyUnforgeable]`; constants read-only, non-configurable, on both
//!   the interface object and the prototype.
//! - `X.prototype[Symbol.toStringTag]` is a read-only, non-enumerable string.
//! - `[Global]` members live on the global itself; interface objects,
//!   aliases, legacy factories and namespaces are non-enumerable global
//!   properties in Chrome's order.
//! - Every function is a native V8 function (`function x() { [native
//!   code] }`), never a JS closure.
//!
//! Unregistered members get stubs: getters return a type-shaped default
//! (`undefined`, `null`, `false`, `0` or `""`), setters do nothing except
//! for `[Replaceable]` (shadowing data property) and `[PutForwards]`
//! (forwarding), operations throw a `NotSupportedError` DOMException, and
//! constructors throw `TypeError: Illegal constructor`.
//!
//! After a realm is created a short fixup script runs once in it (before
//! any page script): it moves members Chrome installs late (after
//! `constructor`, or after the engine's late globals) to the end, adds
//! `Symbol.unscopables` objects, links `DOMException.prototype` to
//! `Error.prototype` and deletes engine globals Chrome does not expose
//! (`SharedArrayBuffer` without cross-origin isolation). It leaves no trace.

use std::collections::HashMap;
use std::fmt::Write;
use std::rc::Rc;

use lb_js::{
    Engine, HostError, HostFn, HostRef, InterfaceId, InterfaceSpec, Intrinsic, JsError, JsValue,
    Member, ModuleLoader, PropAttrs, RealmId, RealmOptions, ScriptOrigin, WellKnownSymbol, World,
    v8,
};
use lb_persona::BrowserFamily;

use crate::desc::{
    ArrayIntrinsic, AttrDesc, AttrType, ConstValue, GlobalProp, InterfaceDesc, NamespaceDesc,
    OpDesc, OpKind, Slot, SymbolIterator, Table,
};
use crate::dom_exception::{DomExceptions, register_defaults};
use crate::registry::{ImplKind, Registry, host_fn};

/// Why bindings could not be defined.
#[derive(Debug, Clone, PartialEq)]
pub enum InstallError {
    /// No table for this family in this build.
    NoTable(BrowserFamily),
    /// A table names an interface that is not defined before it.
    Unknown(&'static str),
    /// Creating the realm or running the fixup failed.
    Js(JsError),
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstallError::NoTable(fam) => write!(f, "no binding table for {}", fam.as_str()),
            InstallError::Unknown(n) => write!(f, "binding table references unknown interface {n}"),
            InstallError::Js(e) => write!(f, "realm setup failed: {e}"),
        }
    }
}

impl std::error::Error for InstallError {}

impl From<JsError> for InstallError {
    fn from(e: JsError) -> Self {
        InstallError::Js(e)
    }
}

/// How to create a window realm.
#[derive(Clone, Default)]
pub struct WindowRealmOptions {
    /// Host object behind the window global (its `HostRef` is then what
    /// `this_host()` returns in window members).
    pub window_host: Option<HostRef>,
    /// Main world, or an isolated world for CDP utility scripts.
    pub world: Option<World>,
    pub module_loader: Option<ModuleLoader>,
    /// The document is not a secure context (plain `http:` on a
    /// non-loopback host, `about:blank` of such a page): `[SecureContext]`
    /// members and interfaces are removed. Default: secure.
    pub insecure: bool,
}

impl std::fmt::Debug for WindowRealmOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowRealmOptions")
            .field("window_host", &self.window_host)
            .field("world", &self.world)
            .field("module_loader", &self.module_loader.is_some())
            .field("insecure", &self.insecure)
            .finish()
    }
}

struct Inner {
    table: &'static Table,
    ids: HashMap<&'static str, InterfaceId>,
    window: InterfaceId,
    fixup: String,
    /// The global-object part of `fixup`, for snapshot realms.
    globals_fixup: String,
    /// Appended to `fixup` for non-secure realms.
    insecure_fixup: String,
    dom_exceptions: DomExceptions,
}

/// All interfaces of one table, defined on one engine. Cheap to clone.
/// Create realms with [`Bindings::new_window_realm`].
#[derive(Clone)]
pub struct Bindings(Rc<Inner>);

impl std::fmt::Debug for Bindings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bindings")
            .field("interfaces", &self.0.ids.len())
            .finish()
    }
}

/// The table for a family, if this build has one.
pub fn table_for(family: BrowserFamily) -> Option<&'static Table> {
    match family {
        BrowserFamily::Chrome => Some(&crate::generated::chromium::TABLE),
        BrowserFamily::Firefox => None,
    }
}

impl Bindings {
    /// Define every interface of the engine persona's family table. Call
    /// once per engine, after registering implementations.
    pub fn define(engine: &mut Engine, registry: &Registry) -> Result<Bindings, InstallError> {
        let family = engine.persona().family;
        let table = table_for(family).ok_or(InstallError::NoTable(family))?;
        Self::define_table(engine, registry, table)
    }

    /// Like [`Bindings::define`] with an explicit table.
    pub fn define_table(
        engine: &mut Engine,
        registry: &Registry,
        table: &'static Table,
    ) -> Result<Bindings, InstallError> {
        let mut registry = registry.clone();
        register_defaults(&mut registry);
        register_window_defaults(&mut registry);
        let dom = registry.dom_exceptions();
        let stubs = Stubs::new(dom.clone());
        let mut ids: HashMap<&'static str, InterfaceId> =
            HashMap::with_capacity(table.interfaces.len());
        let mut global_desc = None;

        for desc in table.interfaces {
            if desc.is_global && desc.name == "Window" {
                // Defined last: its instance holds every interface object.
                global_desc = Some(desc);
                continue;
            }
            let spec = interface_spec(table, desc, &registry, &stubs, &ids)?;
            let id = engine.define_interface(spec);
            ids.insert(desc.name, id);
            if desc.name == "DOMException" {
                dom.set_interface(id);
            }
        }

        // Interfaces the table does not carry because they have no
        // interface object: WebGL extension objects. Only reachable through
        // `getExtension`.
        if table.interface("WebGLRenderingContext").is_some() {
            for (name, spec) in crate::rt::extension_specs(&registry) {
                ids.entry(name)
                    .or_insert_with(|| engine.define_interface(spec));
            }
        }

        // Legacy factory functions (`Image`, `Option`, `Audio`).
        let mut factories: HashMap<&'static str, InterfaceId> = HashMap::new();
        for p in table.window_props {
            if let GlobalProp::LegacyFactory { name, iface } = *p {
                let desc = table.interface(iface).ok_or(InstallError::Unknown(iface))?;
                let mut spec = InterfaceSpec::new(name);
                spec.to_string_tag = false;
                match registry.get(iface, name, ImplKind::LegacyFactory) {
                    Some(f) => {
                        spec = spec.constructor(u32::from(desc.legacy_factory_length), f.clone())
                    }
                    None => spec.length = u32::from(desc.legacy_factory_length),
                }
                factories.insert(name, engine.define_interface(spec));
            }
        }

        let window_desc = global_desc.ok_or(InstallError::Unknown("Window"))?;
        ids.insert(window_desc.name, engine.next_interface_id());
        let mut spec = interface_spec(table, window_desc, &registry, &stubs, &ids)?;
        spec.instance = global_members(table, window_desc, &registry, &stubs, &ids, &factories)?;
        spec.immutable_proto = true;
        let window = engine.define_interface(spec);
        debug_assert_eq!(ids.get(window_desc.name), Some(&window));

        let (fixup, globals_fixup) = fixup_script(table, &ids);
        let insecure_fixup = insecure_script(table, &ids);
        // With a startup snapshot whose interfaces all matched, window
        // realms are deserialized with the fixup already run.
        engine.use_snapshot_context(window, &fixup);
        Ok(Bindings(Rc::new(Inner {
            table,
            ids,
            window,
            fixup,
            globals_fixup,
            insecure_fixup,
            dom_exceptions: dom,
        })))
    }

    /// A V8 startup snapshot of these bindings for `registry` (which must
    /// register the same implementations, in the same order, as the
    /// registries later used with [`Engine::with_snapshot`]): every
    /// interface template plus one window context with the fixup run. No
    /// persona data goes in: the engine uses the default persona of the
    /// family, and the specs only carry persona-independent values (a
    /// differing spec makes the restoring engine build normally).
    ///
    /// The blob lives for the rest of the process and becomes its V8
    /// snapshot (see `lb_js::Engine::create_snapshot`); a process makes at
    /// most one, before any other engine exists.
    pub fn create_snapshot(
        family: BrowserFamily,
        registry: &Registry,
    ) -> Result<&'static [u8], InstallError> {
        let persona = match family {
            BrowserFamily::Chrome => lb_persona::Persona::default_chrome(),
            BrowserFamily::Firefox => return Err(InstallError::NoTable(family)),
        };
        let mut engine = Engine::snapshot_creator(std::sync::Arc::new(persona))
            .map_err(|e| InstallError::Js(JsError::new(lb_js::JsErrorKind::Host, e.to_string())))?;
        let bindings = Bindings::define(&mut engine, registry)?;
        let window = bindings.window_interface();
        let fixup = bindings.0.fixup.clone();
        Ok(engine.create_snapshot(window, &fixup)?)
    }

    /// Interface id by name (`"HTMLDivElement"`), for `Engine::wrap` and
    /// `Arg::Host`.
    pub fn interface_id(&self, name: &str) -> Option<InterfaceId> {
        self.0.ids.get(name).copied()
    }

    /// The global interface (`Window`).
    pub fn window_interface(&self) -> InterfaceId {
        self.0.window
    }

    pub fn table(&self) -> &'static Table {
        self.0.table
    }

    /// Number of defined interfaces (legacy factories excluded).
    pub fn len(&self) -> usize {
        self.0.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.ids.is_empty()
    }

    /// The `DOMException` factory these bindings throw with.
    pub fn dom_exceptions(&self) -> DomExceptions {
        self.0.dom_exceptions.clone()
    }

    /// Create a realm whose global is the window, with every interface,
    /// alias, namespace and window member in Chrome's order.
    pub fn new_window_realm(
        &self,
        engine: &mut dyn lb_js::RealmOps,
        options: WindowRealmOptions,
    ) -> Result<RealmId, JsError> {
        let realm = engine.new_realm(RealmOptions {
            global: Some(self.0.window),
            global_host: options.window_host,
            world: options.world.unwrap_or(World::Main),
            module_loader: options.module_loader,
        })?;
        let mut result = if engine.is_snapshot_realm(realm) {
            engine.evaluate(realm, &self.0.globals_fixup, ScriptOrigin::new(""))
        } else {
            engine.evaluate(realm, &self.0.fixup, ScriptOrigin::new(""))
        };
        if result.is_ok() && options.insecure {
            result = engine.evaluate(realm, &self.0.insecure_fixup, ScriptOrigin::new(""));
        }
        if let Err(e) = result {
            engine.drop_realm(realm);
            return Err(e);
        }
        Ok(realm)
    }
}

/// Define the bindings for the engine's persona and create one window
/// realm: the usual first step for a page.
pub fn install_window_realm(
    engine: &mut Engine,
    registry: &Registry,
    options: WindowRealmOptions,
) -> Result<(Bindings, RealmId), InstallError> {
    let bindings = Bindings::define(engine, registry)?;
    let realm = bindings.new_window_realm(engine, options)?;
    Ok((bindings, realm))
}

/// `window`, `self`, `frames`, `top` and `parent` return the global
/// itself unless registered (right for a top-level window; frames
/// override `top` / `parent`).
fn register_window_defaults(reg: &mut Registry) {
    for name in ["window", "self", "frames", "top", "parent"] {
        if reg.get("Window", name, ImplKind::Getter).is_none() {
            reg.getter(
                "Window",
                name,
                host_fn(|cx| {
                    let this = cx.v8_args().this();
                    cx.ret_v8(this.into());
                    Ok(())
                }),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Stubs
// ---------------------------------------------------------------------------

/// Shared stub functions (one per behaviour, cloned per member).
struct Stubs {
    dom: DomExceptions,
    get_undefined: HostFn,
    get_null: HostFn,
    get_false: HostFn,
    get_zero: HostFn,
    get_empty: HostFn,
    set_ignore: HostFn,
}

impl Stubs {
    fn new(dom: DomExceptions) -> Stubs {
        Stubs {
            dom,
            get_undefined: host_fn(|_| Ok(())),
            get_null: host_fn(|cx| cx.ret(lb_js::Arg::Null)),
            get_false: host_fn(|cx| cx.ret(false)),
            get_zero: host_fn(|cx| cx.ret(0)),
            get_empty: host_fn(|cx| cx.ret("")),
            set_ignore: host_fn(|_| Ok(())),
        }
    }

    fn getter(&self, ty: AttrType) -> HostFn {
        match ty {
            AttrType::Other => self.get_undefined.clone(),
            AttrType::Null => self.get_null.clone(),
            AttrType::Bool => self.get_false.clone(),
            AttrType::Number => self.get_zero.clone(),
            AttrType::String => self.get_empty.clone(),
        }
    }

    fn operation(&self, iface: &'static str, name: &'static str) -> HostFn {
        let dom = self.dom.clone();
        host_fn(move |_| {
            Err(dom.error(
                "NotSupportedError",
                format!("Failed to execute '{name}' on '{iface}': Not supported."),
            ))
        })
    }
}

/// `[Replaceable]` setter: define an own data property on `this`.
fn replaceable_setter(name: &'static str) -> HostFn {
    host_fn(move |cx| {
        let args = cx.v8_args();
        let this = args.this();
        let value = args.get(0);
        let scope = cx.scope();
        let key = v8::String::new(scope, name).ok_or(HostError::Thrown)?;
        this.create_data_property(scope, key.into(), value)
            .ok_or(HostError::Thrown)?;
        Ok(())
    })
}

/// `[PutForwards=fwd]` setter: `this[name][fwd] = value`.
fn put_forwards_setter(iface: &'static str, name: &'static str, fwd: &'static str) -> HostFn {
    host_fn(move |cx| {
        let args = cx.v8_args();
        let this = args.this();
        let value = args.get(0);
        let scope = cx.scope();
        let key = v8::String::new(scope, name).ok_or(HostError::Thrown)?;
        let target = this.get(scope, key.into()).ok_or(HostError::Thrown)?;
        let Ok(target) = v8::Local::<v8::Object>::try_from(target) else {
            return Err(HostError::type_error(format!(
                "Failed to set the '{name}' property on '{iface}': The object is not an object."
            )));
        };
        let fkey = v8::String::new(scope, fwd).ok_or(HostError::Thrown)?;
        target
            .set(scope, fkey.into(), value)
            .ok_or(HostError::Thrown)?;
        Ok(())
    })
}

// ---------------------------------------------------------------------------
// Spec building
// ---------------------------------------------------------------------------

fn const_member(name: &'static str, v: ConstValue) -> Member {
    let value = match v {
        ConstValue::Number(n) => JsValue::Number(n),
        ConstValue::Bool(b) => JsValue::Bool(b),
    };
    Member::constant(name, value)
}

fn attr_member(
    iface: &'static str,
    a: &AttrDesc,
    registry: &Registry,
    stubs: &Stubs,
    statics: bool,
) -> Member {
    let (gk, sk) = if statics {
        (ImplKind::StaticGetter, ImplKind::StaticSetter)
    } else {
        (ImplKind::Getter, ImplKind::Setter)
    };
    let get = registry
        .get(iface, a.name, gk)
        .cloned()
        .unwrap_or_else(|| stubs.getter(a.ty));
    let set = a.has_setter().then(|| {
        registry.get(iface, a.name, sk).cloned().unwrap_or_else(|| {
            if let Some(fwd) = a.put_forwards {
                put_forwards_setter(iface, a.name, fwd)
            } else if a.replaceable {
                replaceable_setter(a.name)
            } else {
                stubs.set_ignore.clone()
            }
        })
    });
    Member::accessor(a.name, Some(get), set).with_attrs(PropAttrs {
        writable: true,
        enumerable: a.enumerable,
        configurable: !a.unforgeable,
    })
}

fn op_member(
    desc_name: &'static str,
    o: &OpDesc,
    d: Option<&InterfaceDesc>,
    registry: &Registry,
    stubs: &Stubs,
    statics: bool,
) -> Member {
    let attrs = PropAttrs {
        writable: !o.unforgeable,
        enumerable: o.enumerable,
        configurable: !o.unforgeable,
    };
    if let OpKind::Array(which) = o.kind {
        let intrinsic = match which {
            ArrayIntrinsic::Entries => Intrinsic::ArrayProtoEntries,
            ArrayIntrinsic::Keys => Intrinsic::ArrayProtoKeys,
            ArrayIntrinsic::Values => Intrinsic::ArrayProtoValues,
            ArrayIntrinsic::ForEach => Intrinsic::ArrayProtoForEach,
        };
        return Member::intrinsic(o.name, intrinsic).with_attrs(attrs);
    }
    let kind = if statics {
        ImplKind::StaticMethod
    } else {
        ImplKind::Method
    };
    let f = registry.get(desc_name, o.name, kind).cloned().or_else(|| {
        // `toString` of `stringifier attribute x` is x's getter.
        let attr = d.and_then(|d| d.stringifier_attr)?;
        (o.kind == OpKind::Stringifier)
            .then(|| registry.get(desc_name, attr, ImplKind::Getter).cloned())
            .flatten()
    });
    let f = f.unwrap_or_else(|| stubs.operation(desc_name, o.name));
    Member::method(o.name, u32::from(o.length), f).with_attrs(attrs)
}

fn slot_member(
    d: &InterfaceDesc,
    s: Slot,
    registry: &Registry,
    stubs: &Stubs,
    statics: bool,
) -> Member {
    match s {
        Slot::Const(i) => {
            let (n, v) = d.constants[i as usize];
            const_member(n, v)
        }
        Slot::Attr(i) => attr_member(d.name, &d.attributes[i as usize], registry, stubs, statics),
        Slot::Op(i) => op_member(
            d.name,
            &d.operations[i as usize],
            Some(d),
            registry,
            stubs,
            statics,
        ),
    }
}

/// Symbol members after the string-keyed ones: `@@toStringTag` first (as in
/// Chrome), then `@@iterator` / `@@asyncIterator`.
fn symbol_members(d: &InterfaceDesc, out: &mut Vec<Member>) {
    let hidden = PropAttrs {
        writable: true,
        enumerable: false,
        configurable: true,
    };
    if !d.to_string_tag.is_empty() {
        out.push(
            Member::value(
                WellKnownSymbol::ToStringTag,
                JsValue::String(d.to_string_tag.to_owned()),
            )
            .with_attrs(PropAttrs {
                writable: false,
                enumerable: false,
                configurable: true,
            }),
        );
    }
    match d.iterator {
        SymbolIterator::None => {}
        SymbolIterator::ArrayValues => out.push(
            Member::intrinsic(WellKnownSymbol::Iterator, Intrinsic::ArrayProtoValues)
                .with_attrs(hidden),
        ),
        SymbolIterator::Alias(of) => {
            out.push(Member::alias(WellKnownSymbol::Iterator, of).with_attrs(hidden))
        }
    }
    if let Some(of) = d.async_iterator {
        out.push(Member::alias(WellKnownSymbol::AsyncIterator, of).with_attrs(hidden));
    }
}

fn interface_spec(
    table: &'static Table,
    d: &'static InterfaceDesc,
    registry: &Registry,
    stubs: &Stubs,
    ids: &HashMap<&'static str, InterfaceId>,
) -> Result<InterfaceSpec, InstallError> {
    let mut spec = InterfaceSpec::new(d.name);
    if let Some(p) = d.parent {
        spec = spec.parent(*ids.get(p).ok_or(InstallError::Unknown(p))?);
    }
    match registry.get(d.name, "", ImplKind::Constructor) {
        Some(f) if d.has_constructor => {
            spec = spec.constructor(u32::from(d.constructor_length), f.clone())
        }
        _ => spec.length = u32::from(d.constructor_length),
    }
    spec.to_string_tag = false;
    spec.statics = d
        .static_order
        .iter()
        .map(|s| slot_member(d, *s, registry, stubs, true))
        .collect();
    let mut proto: Vec<Member> = d
        .proto_order
        .iter()
        .map(|s| slot_member(d, *s, registry, stubs, false))
        .collect();
    symbol_members(d, &mut proto);
    spec.prototype = proto;
    if !d.is_global {
        // Instance members (`[LegacyUnforgeable]`) of ancestors are own
        // properties of every instance too (`isTrusted` on a MouseEvent).
        let mut chain = vec![d];
        let mut parent = d.parent;
        while let Some(p) = parent.and_then(|p| table.interface(p)) {
            chain.push(p);
            parent = p.parent;
        }
        spec.instance = chain
            .iter()
            .rev()
            .flat_map(|a| a.instance_order.iter().map(move |s| (*a, s)))
            .map(|(a, s)| slot_member(a, *s, registry, stubs, false))
            .collect();
    }
    if let Some(f) = registry.named_getter_for(d.name) {
        spec = spec.named_getter(f.clone());
        if let Some(f) = registry.named_setter_for(d.name) {
            spec = spec.named_setter(f.clone());
        }
        if let Some(f) = registry.named_deleter_for(d.name) {
            spec = spec.named_deleter(f.clone());
        }
        if let Some(f) = registry.named_enumerator_for(d.name) {
            spec = spec.named_enumerator(f.clone());
        }
        spec.named_unenumerable = registry.is_named_unenumerable(d.name);
    }
    if let Some(f) = registry.indexed_getter_for(d.name) {
        spec = spec.indexed_getter(f.clone());
        if let Some(f) = registry.indexed_length_for(d.name) {
            spec = spec.indexed_length(f.clone());
        }
    }
    Ok(spec)
}

fn namespace_member(n: &'static NamespaceDesc, registry: &Registry, stubs: &Stubs) -> Member {
    let members = n
        .order
        .iter()
        .map(|s| match *s {
            Slot::Const(i) => {
                let (name, v) = n.constants[i as usize];
                const_member(name, v)
            }
            Slot::Attr(i) => attr_member(n.name, &n.attributes[i as usize], registry, stubs, false),
            Slot::Op(i) => op_member(
                n.name,
                &n.operations[i as usize],
                None,
                registry,
                stubs,
                false,
            ),
        })
        .collect();
    Member::namespace(n.name, n.to_string_tag, members)
}

/// Own properties of the global, in table order.
fn global_members(
    table: &'static Table,
    window: &'static InterfaceDesc,
    registry: &Registry,
    stubs: &Stubs,
    ids: &HashMap<&'static str, InterfaceId>,
    factories: &HashMap<&'static str, InterfaceId>,
) -> Result<Vec<Member>, InstallError> {
    let mut out = Vec::with_capacity(table.window_props.len());
    for p in table.window_props {
        match *p {
            GlobalProp::Builtin(_) | GlobalProp::Other(_) => {}
            GlobalProp::Interface(n) => {
                // `Window` itself resolves to the id the window is about to
                // get (`Engine::next_interface_id`).
                out.push(Member::interface(
                    n,
                    *ids.get(n).ok_or(InstallError::Unknown(n))?,
                ));
            }
            GlobalProp::Alias { name, target } => out.push(Member::interface(
                name,
                *ids.get(target).ok_or(InstallError::Unknown(target))?,
            )),
            GlobalProp::LegacyFactory { name, .. } => out.push(Member::interface(
                name,
                *factories.get(name).ok_or(InstallError::Unknown(name))?,
            )),
            GlobalProp::Namespace(n) => {
                let ns = table.namespace(n).ok_or(InstallError::Unknown(n))?;
                out.push(namespace_member(ns, registry, stubs));
            }
            GlobalProp::Attr(n) => {
                if let Some(i) = window.attributes.iter().position(|a| a.name == n) {
                    out.push(slot_member(
                        window,
                        Slot::Attr(i as u16),
                        registry,
                        stubs,
                        false,
                    ));
                }
            }
            GlobalProp::Op(n) => {
                if let Some(i) = window.operations.iter().position(|o| o.name == n) {
                    out.push(slot_member(
                        window,
                        Slot::Op(i as u16),
                        registry,
                        stubs,
                        false,
                    ));
                }
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Fixup script
// ---------------------------------------------------------------------------

fn js_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn js_list<'a>(names: impl IntoIterator<Item = &'a str>) -> String {
    let v: Vec<String> = names.into_iter().map(js_str).collect();
    format!("[{}]", v.join(","))
}

/// The per-realm fixup (see the module docs). Interfaces are reached
/// through captured global values before anything else runs.
///
/// Returns the script and its global-object part alone. A realm
/// deserialized from the startup snapshot already ran the full script at
/// snapshot time, but V8 re-installs its flag-gated globals (`Temporal`,
/// `DisposableStack`, `Float16Array`, `SharedArrayBuffer`, `WebAssembly`,
/// ...) at the end of every new context, so the global part runs again.
fn fixup_script(table: &Table, ids: &HashMap<&'static str, InterfaceId>) -> (String, String) {
    const PRELUDE: &str = "(function(){'use strict';const g=globalThis,O=Object,gd=O.getOwnPropertyDescriptor,dp=O.defineProperty;\
         function L(o,ks){for(const k of ks){const d=gd(o,k);if(d){delete o[k];dp(o,k,d);}}}\
         function P(n){const d=gd(g,n);return d&&d.value&&d.value.prototype;}";
    let mut s = String::from(PRELUDE);
    let on_window = |name: &str| {
        table
            .window_props
            .iter()
            .any(|p| matches!(p, GlobalProp::Interface(n) if *n == name))
    };
    for d in table.interfaces {
        if !ids.contains_key(d.name) || !on_window(d.name) {
            continue;
        }
        let late: Vec<&str> = d
            .proto_order
            .iter()
            .filter(|s| match **s {
                Slot::Attr(i) => d.attributes[i as usize].late,
                Slot::Op(i) => d.operations[i as usize].late,
                Slot::Const(_) => false,
            })
            .map(|s| d.slot_name(*s))
            .collect();
        if !late.is_empty() {
            let _ = write!(s, "L(P({}),{});", js_str(d.name), js_list(late));
        }
        if !d.unscopables.is_empty() {
            let _ = write!(
                s,
                "{{const u=O.create(null);for(const k of {})u[k]=true;dp(P({}),Symbol.unscopables,{{value:u,writable:false,enumerable:false,configurable:true}});}}",
                js_list(d.unscopables.iter().copied()),
                js_str(d.name)
            );
        }
        if let Some(b) = d.builtin_parent {
            let _ = write!(
                s,
                "O.setPrototypeOf(P({}),P({}));",
                js_str(d.name),
                js_str(b)
            );
        }
    }
    // Legacy factories inherit from Function.prototype, not from the
    // element interface (nothing to do: they have no parent).
    let globals_at = s.len();
    let late_globals = table.window_props[table.window_late_start.min(table.window_props.len())..]
        .iter()
        .map(GlobalProp::name);
    let _ = write!(s, "L(g,{});", js_list(late_globals));
    // Engine globals Chrome does not expose here (e.g. `SharedArrayBuffer`
    // outside cross-origin isolation) are removed.
    let _ = write!(
        s,
        "{{const k=new Set({});for(const n of O.getOwnPropertyNames(g))if(!k.has(n)){{const d=gd(g,n);if(d&&d.configurable)delete g[n];}}}}",
        js_list(table.window_props.iter().map(GlobalProp::name))
    );
    s.push_str("})()");
    let globals = format!("{PRELUDE}{}", &s[globals_at..]);
    (s, globals)
}

/// Removes what only secure contexts have.
fn insecure_script(table: &Table, ids: &HashMap<&'static str, InterfaceId>) -> String {
    let mut s = String::from(
        "(function(){'use strict';const g=globalThis,gd=Object.getOwnPropertyDescriptor;\
         function I(n){const d=gd(g,n);return d&&d.value;}\
         function D(o,ks){if(o)for(const k of ks)delete o[k];}",
    );
    for d in table.interfaces {
        if !ids.contains_key(d.name) {
            continue;
        }
        let secure = |s: &Slot| match *s {
            Slot::Attr(i) => d.attributes[i as usize].secure_context,
            Slot::Op(i) => d.operations[i as usize].secure_context,
            Slot::Const(_) => false,
        };
        if d.is_global {
            continue;
        }
        let proto: Vec<&str> = d
            .proto_order
            .iter()
            .filter(|s| secure(s))
            .map(|s| d.slot_name(*s))
            .collect();
        if !proto.is_empty() {
            let _ = write!(
                s,
                "{{const i=I({});D(i&&i.prototype,{});}}",
                js_str(d.name),
                js_list(proto)
            );
        }
        let statics: Vec<&str> = d
            .static_order
            .iter()
            .filter(|s| secure(s))
            .map(|s| d.slot_name(*s))
            .collect();
        if !statics.is_empty() {
            let _ = write!(s, "D(I({}),{});", js_str(d.name), js_list(statics));
        }
    }
    // Interface objects and window members, last: the lookups above need them.
    let _ = write!(
        s,
        "D(g,{});",
        js_list(table.window_secure_only.iter().copied())
    );
    s.push_str("})()");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixup_mentions_late_members() {
        let mut ids = HashMap::new();
        for (i, d) in crate::generated::chromium::INTERFACES.iter().enumerate() {
            ids.insert(d.name, InterfaceId(i as u32));
        }
        let (s, globals) = fixup_script(&crate::generated::chromium::TABLE, &ids);
        assert!(globals.starts_with("(function(){") && globals.ends_with("})()"));
        assert!(globals.contains("L(g,[") && !globals.contains("XMLHttpRequest\"),["));
        assert!(
            s.contains(r#"L(P("XMLHttpRequest"),["responseXML","#),
            "{s}"
        );
        assert!(s.contains("Symbol.unscopables"));
        assert!(s.contains(r#"O.setPrototypeOf(P("DOMException"),P("Error"))"#));
    }
}

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
//! Interfaces are defined lazily and the window's interface objects are
//! lazy data properties, as in Blink (ADR 0016): an interface's spec and
//! templates are made when something first needs them, and its interface
//! object when first reached in a realm. What V8 templates cannot express
//! is an [`InterfaceFixup`] the engine applies as each interface object is
//! made: members Chrome installs late (after `constructor`) move to the
//! end, `Symbol.unscopables` objects, `DOMException.prototype` inheriting
//! from `Error.prototype`, and the realm's removals (`[SecureContext]`
//! members of a non-secure context, OS-gated members). Window members
//! Chrome installs after the engine's late globals are put last when the
//! global is set up (`InterfaceSpec::reorder_last`). Once per realm,
//! before any page script, engine globals Chrome does not expose
//! (`SharedArrayBuffer` without cross-origin isolation) are deleted. None
//! of it leaves a trace.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt::Write;
use std::rc::Rc;

use lb_js::{
    Engine, HostError, HostFn, HostRef, InterfaceFixup, InterfaceFixups, InterfaceId,
    InterfaceSpec, Intrinsic, JsError, JsValue, Member, ModuleLoader, PropAttrs, RealmId,
    RealmOptions, ScriptOrigin, WellKnownSymbol, World, v8,
};
use lb_persona::BrowserFamily;

use crate::desc::{
    ArrayIntrinsic, AttrDesc, AttrType, ConstValue, GlobalProp, InterfaceDesc, NamespaceDesc,
    OpDesc, OpKind, S, Slot, SymbolIterator, Table,
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
    /// The persona's OS-gated APIs (`PersonaProfile::os_apis`): members of
    /// each gate turned off (`Table::os_gated`) are deleted from the realm,
    /// which keeps the order of the rest. `None` keeps every gated member
    /// (the snapshot's surface).
    pub os_apis: Option<lb_persona::OsApis>,
    /// The caller defines the window's host properties itself
    /// (`GlobalProp::Other`: `chrome`): their slots stay, holding
    /// `undefined` until then, so they sit in Chrome's place. Otherwise
    /// they are removed.
    pub host_slots: bool,
}

impl std::fmt::Debug for WindowRealmOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowRealmOptions")
            .field("window_host", &self.window_host)
            .field("world", &self.world)
            .field("module_loader", &self.module_loader.is_some())
            .field("insecure", &self.insecure)
            .field("os_apis", &self.os_apis)
            .field("host_slots", &self.host_slots)
            .finish()
    }
}

struct Inner {
    table: &'static Table,
    ids: HashMap<&'static str, InterfaceId>,
    window: InterfaceId,
    /// Run in the snapshot context: deletes engine globals Chrome does not
    /// expose (realms do the same natively, see `window_names`).
    globals_fixup: String,
    /// Every own property name of Chrome's window.
    window_names: std::collections::HashSet<&'static str>,
    /// Host property slots (`GlobalProp::Other`), removed unless the
    /// realm keeps them.
    host_slots: Vec<&'static str>,
    /// Per-global interface fixups (late members, unscopables,
    /// `DOMException`'s Error prototype).
    base_fixups: HashMap<InterfaceId, InterfaceFixup>,
    /// Realm fixups by (insecure, OS gates turned off).
    realm_fixups: RefCell<HashMap<(bool, Vec<&'static str>), InterfaceFixups>>,
    /// Deletes the window members only secure contexts have.
    insecure_window: String,
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
    ///
    /// Interfaces are defined lazily (`Engine::define_interface_lazy`):
    /// their specs and templates are made when a realm first needs them.
    /// The window interface is defined at once.
    pub fn define_table(
        engine: &mut Engine,
        registry: &Registry,
        table: &'static Table,
    ) -> Result<Bindings, InstallError> {
        let mut registry = registry.clone();
        // DOM node refs carry their document (`rt::node_key`): one wrapper
        // per node in the main world, whichever frame's realm reaches it.
        #[cfg(feature = "chrome")]
        engine.set_shared_hosts(0xff, crate::rt::K_NODE);
        register_defaults(&mut registry);
        register_window_defaults(&mut registry);
        registry.freeze();
        let dom = registry.dom_exceptions();
        let registry = Rc::new(registry);
        let stubs = Rc::new(Stubs::new(dom.clone()));
        // Shared with the lazy specs, which read it once it is complete.
        let shared: Rc<RefCell<HashMap<&'static str, InterfaceId>>> =
            Rc::new(RefCell::new(HashMap::with_capacity(table.interfaces.len())));
        let mut global_desc = None;

        for desc in table.interfaces {
            if desc.is_global && desc.name == "Window" {
                // Defined last: its instance holds every interface object.
                global_desc = Some(desc);
                continue;
            }
            // The only way `interface_spec` fails: checked now, so the
            // lazy spec cannot.
            if let Some(p) = desc.parent.map(S::get)
                && !shared.borrow().contains_key(p)
            {
                return Err(InstallError::Unknown(p));
            }
            let (reg, st, ids) = (registry.clone(), stubs.clone(), shared.clone());
            let id = engine.define_interface_lazy(Box::new(move || {
                let ids = ids.borrow();
                interface_spec(table, desc, &reg, &st, &ids)
                    .unwrap_or_else(|_| InterfaceSpec::new(desc.name.get()))
            }));
            shared.borrow_mut().insert(desc.name.get(), id);
            if desc.name == "DOMException" {
                dom.set_interface(id);
            }
        }

        // Interfaces the table does not carry because they have no
        // interface object: WebGL extension objects. Only reachable through
        // `getExtension`.
        if table.interface("WebGLRenderingContext").is_some() {
            for name in crate::rt::extension_names() {
                if !shared.borrow().contains_key(name) {
                    let reg = registry.clone();
                    let id = engine.define_interface_lazy(Box::new(move || {
                        crate::rt::extension_spec(&reg, name)
                    }));
                    shared.borrow_mut().insert(name, id);
                }
            }
        }

        // Legacy factory functions (`Image`, `Option`, `Audio`).
        let mut factories: HashMap<&'static str, InterfaceId> = HashMap::new();
        for p in table.window_props {
            if let GlobalProp::LegacyFactory { name, iface } = *p {
                let (name, iface) = (name.get(), iface.get());
                let desc = table.interface(iface).ok_or(InstallError::Unknown(iface))?;
                let mut spec = InterfaceSpec::new(name);
                spec.to_string_tag = false;
                // `Image.prototype === HTMLImageElement.prototype`.
                spec.prototype_from = shared.borrow().get(iface).copied();
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
        shared
            .borrow_mut()
            .insert(window_desc.name.get(), engine.next_interface_id());
        let mut spec = {
            let ids = shared.borrow();
            let mut spec = interface_spec(table, window_desc, &registry, &stubs, &ids)?;
            spec.instance =
                global_members(table, window_desc, &registry, &stubs, &ids, &factories)?;
            spec
        };
        spec.immutable_proto = true;
        // Members Chrome installs after the engine's late globals, in
        // order (with those globals).
        spec.reorder_last = table.window_props
            [table.window_late_start.min(table.window_props.len())..]
            .iter()
            .map(|p| lb_js::PropKey::Name(p.name().to_owned()))
            .collect();
        let window = engine.define_interface(spec);
        let ids = shared.borrow().clone();
        debug_assert_eq!(ids.get(window_desc.name.get()), Some(&window));

        let base_fixups = base_fixups(table, &ids)?;
        let globals_fixup = globals_script(table);
        let insecure_window = insecure_window_script(table);
        // With a startup snapshot whose interfaces all matched, window
        // realms are deserialized with the global set up already.
        let base = Rc::new(base_fixups.clone());
        engine.use_snapshot_context(window, &globals_fixup, Some(&base));
        Ok(Bindings(Rc::new(Inner {
            table,
            ids,
            window,
            globals_fixup,
            window_names: table.window_props.iter().map(GlobalProp::name).collect(),
            host_slots: table
                .window_props
                .iter()
                .filter_map(|p| match p {
                    GlobalProp::Other(n) => Some(n.get()),
                    _ => None,
                })
                .collect(),
            base_fixups,
            realm_fixups: RefCell::new(HashMap::new()),
            insecure_window,
            dom_exceptions: dom,
        })))
    }

    /// A V8 startup snapshot of these bindings for `registry` (which must
    /// register the same implementations, in the same order, as the
    /// registries later used with [`Engine::with_snapshot`]): every
    /// interface template plus one window context with the global set up.
    /// No persona data goes in: the engine uses the default persona of the
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
        let script = bindings.0.globals_fixup.clone();
        let base = Rc::new(bindings.0.base_fixups.clone());
        Ok(engine.create_snapshot(window, &script, Some(base))?)
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
    /// alias, namespace and window member in Chrome's order. Interface
    /// objects are made when first used, with the realm's fixups.
    pub fn new_window_realm(
        &self,
        engine: &mut dyn lb_js::RealmOps,
        options: WindowRealmOptions,
    ) -> Result<RealmId, JsError> {
        let fixups = self.realm_fixups(options.insecure, options.os_apis);
        let realm = engine.new_realm(RealmOptions {
            global: Some(self.0.window),
            global_host: options.window_host,
            world: options.world.unwrap_or(World::Main),
            module_loader: options.module_loader,
            fixups: Some(fixups),
        })?;
        // Engine globals Chrome does not expose here (e.g.
        // `SharedArrayBuffer` outside cross-origin isolation); V8 adds its
        // flag-gated ones to snapshot realms too.
        let names = &self.0.window_names;
        let mut result = engine
            .delete_globals_except(realm, &|n| names.contains(n))
            .map(|_| JsValue::Undefined);
        if result.is_ok() && !options.host_slots && !self.0.host_slots.is_empty() {
            result = engine.evaluate(
                realm,
                &format!(
                    "(function(){{'use strict';const g=globalThis;for(const k of {})delete g[k];}})()",
                    js_list(self.0.host_slots.iter().copied())
                ),
                ScriptOrigin::new(""),
            );
        }
        if result.is_ok() && options.insecure {
            result = engine.evaluate(realm, &self.0.insecure_window, ScriptOrigin::new(""));
        }
        if result.is_ok()
            && let Some(apis) = options.os_apis
            && let Some(script) = os_gate_window_script(self.0.table, apis)
        {
            result = engine.evaluate(realm, &script, ScriptOrigin::new(""));
        }
        if let Err(e) = result {
            engine.drop_realm(realm);
            return Err(e);
        }
        Ok(realm)
    }

    /// The interface fixups of a realm: the base ones plus the removals of
    /// a non-secure context and of the OS gates `apis` turns off. Made once
    /// per combination.
    fn realm_fixups(&self, insecure: bool, apis: Option<lb_persona::OsApis>) -> InterfaceFixups {
        let off: Vec<&'static str> = match apis {
            Some(apis) => {
                let mut v: Vec<&'static str> = self
                    .0
                    .table
                    .os_gated
                    .iter()
                    .filter(|g| !os_gate_on(g.gate.get(), apis))
                    .map(|g| g.gate.get())
                    .collect();
                v.sort_unstable();
                v.dedup();
                v
            }
            None => Vec::new(),
        };
        let key = (insecure, off);
        if let Some(f) = self.0.realm_fixups.borrow().get(&key) {
            return f.clone();
        }
        let mut all = self.0.base_fixups.clone();
        if insecure {
            add_insecure_removals(self.0.table, &self.0.ids, &mut all);
        }
        if let Some(apis) = apis {
            add_os_gate_removals(self.0.table, &self.0.ids, apis, &mut all);
        }
        let f: InterfaceFixups = Rc::new(all);
        self.0.realm_fixups.borrow_mut().insert(key, f.clone());
        f
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
        .get_declared(iface, a.name.get(), gk)
        .cloned()
        .unwrap_or_else(|| stubs.getter(a.ty));
    let set = a.has_setter().then(|| {
        registry
            .get_declared(iface, a.name.get(), sk)
            .cloned()
            .unwrap_or_else(|| {
                if let Some(fwd) = a.put_forwards {
                    put_forwards_setter(iface, a.name.get(), fwd.get())
                } else if a.replaceable {
                    replaceable_setter(a.name.get())
                } else {
                    stubs.set_ignore.clone()
                }
            })
    });
    Member::accessor(a.name.get(), Some(get), set).with_attrs(PropAttrs {
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
        return Member::intrinsic(o.name.get(), intrinsic).with_attrs(attrs);
    }
    let kind = if statics {
        ImplKind::StaticMethod
    } else {
        ImplKind::Method
    };
    let f = registry
        .get_declared(desc_name, o.name.get(), kind)
        .cloned()
        .or_else(|| {
            // `toString` of `stringifier attribute x` is x's getter.
            let attr = d.and_then(|d| d.stringifier_attr)?;
            (o.kind == OpKind::Stringifier)
                .then(|| {
                    registry
                        .get_declared(desc_name, attr.get(), ImplKind::Getter)
                        .cloned()
                })
                .flatten()
        });
    let f = f.unwrap_or_else(|| stubs.operation(desc_name, o.name.get()));
    Member::method(o.name.get(), u32::from(o.length), f).with_attrs(attrs)
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
            const_member(n.get(), v)
        }
        Slot::Attr(i) => attr_member(
            d.name.get(),
            &d.attributes[i as usize],
            registry,
            stubs,
            statics,
        ),
        Slot::Op(i) => op_member(
            d.name.get(),
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
                JsValue::String(d.to_string_tag.get().to_owned()),
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
    let mut spec = InterfaceSpec::new(d.name.get());
    if let Some(p) = d.parent.map(S::get) {
        spec = spec.parent(*ids.get(p).ok_or(InstallError::Unknown(p))?);
    }
    match registry.get(d.name.get(), "", ImplKind::Constructor) {
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
        while let Some(p) = parent.and_then(|p| table.interface(&p)) {
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
    if let Some(f) = registry.named_getter_for(d.name.get()) {
        spec = spec.named_getter(f.clone());
        if let Some(f) = registry.named_setter_for(d.name.get()) {
            spec = spec.named_setter(f.clone());
        }
        if let Some(f) = registry.named_deleter_for(d.name.get()) {
            spec = spec.named_deleter(f.clone());
        }
        if let Some(f) = registry.named_enumerator_for(d.name.get()) {
            spec = spec.named_enumerator(f.clone());
        }
        spec.named_unenumerable = registry.is_named_unenumerable(d.name.get());
    }
    if let Some(f) = registry.indexed_getter_for(d.name.get()) {
        spec = spec.indexed_getter(f.clone());
        if let Some(f) = registry.indexed_length_for(d.name.get()) {
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
                const_member(name.get(), v)
            }
            Slot::Attr(i) => attr_member(
                n.name.get(),
                &n.attributes[i as usize],
                registry,
                stubs,
                false,
            ),
            Slot::Op(i) => op_member(
                n.name.get(),
                &n.operations[i as usize],
                None,
                registry,
                stubs,
                false,
            ),
        })
        .collect();
    Member::namespace(n.name.get(), n.to_string_tag.get(), members)
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
            GlobalProp::Builtin(_) => {}
            // A slot the host fills (`window.chrome`), in Chrome's place.
            GlobalProp::Other(n) => out.push(Member::value(n, JsValue::Undefined)),
            GlobalProp::Interface(n) => {
                let n = n.get();
                // `Window` itself resolves to the id the window is about to
                // get (`Engine::next_interface_id`).
                out.push(Member::interface(
                    n,
                    *ids.get(n).ok_or(InstallError::Unknown(n))?,
                ));
            }
            GlobalProp::Alias { name, target } => out.push(Member::interface(
                name.get(),
                *ids.get(target.get())
                    .ok_or(InstallError::Unknown(target.get()))?,
            )),
            GlobalProp::LegacyFactory { name, .. } => out.push(Member::interface(
                name.get(),
                *factories
                    .get(name.get())
                    .ok_or(InstallError::Unknown(name.get()))?,
            )),
            GlobalProp::Namespace(n) => {
                let ns = table.namespace(&n).ok_or(InstallError::Unknown(n.get()))?;
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

/// Per-global interface fixups, for interfaces with an interface object
/// on the window: members Chrome installs late (after `constructor`),
/// `Symbol.unscopables`, and `DOMException.prototype` inheriting from
/// `Error.prototype`.
fn base_fixups(
    table: &Table,
    ids: &HashMap<&'static str, InterfaceId>,
) -> Result<HashMap<InterfaceId, InterfaceFixup>, InstallError> {
    let mut out = HashMap::new();
    let window = window_interfaces(table);
    for d in table.interfaces {
        let Some(&id) = ids.get(d.name.get()) else {
            continue;
        };
        if !window.contains(d.name.get()) {
            continue;
        }
        let mut fx = InterfaceFixup {
            proto_late: d
                .proto_order
                .iter()
                .filter(|s| match **s {
                    Slot::Attr(i) => d.attributes[i as usize].late,
                    Slot::Op(i) => d.operations[i as usize].late,
                    Slot::Const(_) => false,
                })
                .map(|s| d.slot_name(*s).to_owned())
                .collect(),
            unscopables: d.unscopables.iter().map(|s| s.get().to_owned()).collect(),
            ..Default::default()
        };
        match d.builtin_parent.map(S::get) {
            None => {}
            Some("Error") => fx.error_proto = true,
            Some(other) => return Err(InstallError::Unknown(other)),
        }
        if !fx.is_empty() {
            out.insert(id, fx);
        }
    }
    Ok(out)
}

/// Interfaces whose interface object is on the window.
fn window_interfaces(table: &Table) -> std::collections::HashSet<&'static str> {
    table
        .window_props
        .iter()
        .filter_map(|p| match p {
            GlobalProp::Interface(n) => Some(n.get()),
            _ => None,
        })
        .collect()
}

/// The script every window realm runs once: engine globals Chrome does
/// not expose here (e.g. `SharedArrayBuffer` outside cross-origin
/// isolation) are removed. The script's value lists them and the final
/// own-property count, one per line.
fn globals_script(table: &Table) -> String {
    let mut s = String::from(GLOBALS_PRELUDE);
    let _ = write!(
        s,
        "const k=new Set({}),x=[];for(const n of O.getOwnPropertyNames(g))if(!k.has(n)){{const d=gd(g,n);if(d&&d.configurable){{delete g[n];x.push(n+'\\n');}}}}return x.join('')+O.getOwnPropertyNames(g).length",
        js_list(table.window_props.iter().map(GlobalProp::name))
    );
    s.push_str("})()");
    s
}

/// Start of the globals script.
const GLOBALS_PRELUDE: &str =
    "(function(){'use strict';const g=globalThis,O=Object,gd=O.getOwnPropertyDescriptor;";

/// Removal fixups of a non-secure context: `[SecureContext]` members of
/// every interface on the window.
fn add_insecure_removals(
    table: &Table,
    ids: &HashMap<&'static str, InterfaceId>,
    out: &mut HashMap<InterfaceId, InterfaceFixup>,
) {
    let window = window_interfaces(table);
    for d in table.interfaces {
        let Some(&id) = ids.get(d.name.get()) else {
            continue;
        };
        if d.is_global || !window.contains(d.name.get()) {
            continue;
        }
        let secure = |s: &Slot| match *s {
            Slot::Attr(i) => d.attributes[i as usize].secure_context,
            Slot::Op(i) => d.operations[i as usize].secure_context,
            Slot::Const(_) => false,
        };
        let proto: Vec<String> = d
            .proto_order
            .iter()
            .filter(|s| secure(s))
            .map(|s| d.slot_name(*s).to_owned())
            .collect();
        let statics: Vec<String> = d
            .static_order
            .iter()
            .filter(|s| secure(s))
            .map(|s| d.slot_name(*s).to_owned())
            .collect();
        if proto.is_empty() && statics.is_empty() {
            continue;
        }
        let fx = out.entry(id).or_default();
        fx.remove_proto.extend(proto);
        fx.remove_static.extend(statics);
    }
}

/// Deletes the window members only secure contexts have.
fn insecure_window_script(table: &Table) -> String {
    format!(
        "(function(){{'use strict';const g=globalThis;for(const k of {})delete g[k];}})()",
        js_list(table.window_secure_only.iter().map(|s| s.get()))
    )
}

/// Whether an OS gate (`Table::os_gated`) is on for `apis`. Unknown gates
/// stay on.
pub fn os_gate_on(gate: &str, apis: lb_persona::OsApis) -> bool {
    match gate {
        "share" => apis.share,
        "bluetooth" => apis.bluetooth,
        _ => true,
    }
}

/// Removal fixups for the OS gates `apis` turns off, on interfaces with an
/// interface object on the window (window members: [`os_gate_window_script`]).
fn add_os_gate_removals(
    table: &Table,
    ids: &HashMap<&'static str, InterfaceId>,
    apis: lb_persona::OsApis,
    out: &mut HashMap<InterfaceId, InterfaceFixup>,
) {
    let window = window_interfaces(table);
    for g in table.os_gated {
        if os_gate_on(g.gate.get(), apis)
            || g.owner.get() == "Window"
            || !window.contains(g.owner.get())
        {
            continue;
        }
        let Some(&id) = ids.get(g.owner.get()) else {
            continue;
        };
        let is_static = table.interface(g.owner.get()).is_some_and(|d| {
            d.static_order
                .iter()
                .any(|s| d.slot_name(*s) == g.name.get())
        });
        let fx = out.entry(id).or_default();
        if is_static {
            fx.remove_static.push(g.name.get().to_owned());
        } else {
            fx.remove_proto.push(g.name.get().to_owned());
        }
    }
}

/// The script that deletes the window members of the OS gates `apis`
/// turns off, or `None` when there are none. Deleting keeps the order of
/// everything else.
fn os_gate_window_script(table: &Table, apis: lb_persona::OsApis) -> Option<String> {
    let window: Vec<&str> = table
        .os_gated
        .iter()
        .filter(|g| !os_gate_on(g.gate.get(), apis) && g.owner.get() == "Window")
        .map(|g| g.name.get())
        .collect();
    if window.is_empty() {
        return None;
    }
    Some(format!(
        "(function(){{'use strict';const g=globalThis;for(const k of {})delete g[k];}})()",
        js_list(window)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixups_carry_late_members() {
        let table = &crate::generated::chromium::TABLE;
        let mut ids = HashMap::new();
        for (i, d) in table.interfaces.iter().enumerate() {
            ids.insert(d.name.get(), InterfaceId(i as u32));
        }
        let fx = base_fixups(table, &ids).unwrap();
        let xhr = &fx[&ids["XMLHttpRequest"]];
        assert_eq!(
            xhr.proto_late.first().map(String::as_str),
            Some("responseXML")
        );
        assert!(fx.values().any(|f| !f.unscopables.is_empty()));
        assert!(fx[&ids["DOMException"]].error_proto);
        let globals = globals_script(table);
        assert!(globals.starts_with("(function(){") && globals.ends_with("})()"));
    }
}

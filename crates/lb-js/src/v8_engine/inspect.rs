//! Value inspection for the CDP layer: V8-inspector-style descriptions
//! ([`Engine::inspect`]), by-value serialization ([`Engine::json_by_value`]),
//! evaluation that keeps the thrown value ([`Engine::evaluate_catching`],
//! [`Engine::call_catching`]), promise state ([`Engine::promise_outcome`])
//! and own properties ([`Engine::own_properties`]).
//!
//! All of this is native: no JS runs in the realm except property getters
//! reached while reading values (as in V8's inspector), so nothing is
//! visible to the page.

use super::*;

/// A shallow description of a value, as V8's inspector builds it for a
/// `Runtime.RemoteObject`. Mirrors `lb_cdp::remote::JsValue`.
#[derive(Clone, Debug, PartialEq)]
pub enum Inspected {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    /// Decimal digits, without the `n` suffix.
    BigInt(String),
    String(String),
    /// The symbol's description.
    Symbol(String),
    Function {
        /// `Function`, `AsyncFunction`, `GeneratorFunction`, ...
        class_name: String,
        /// `Function.prototype.toString` text.
        source: String,
    },
    Object(Box<InspectedObject>),
}

/// An object in an [`Inspected`] value.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InspectedObject {
    /// Constructor name (`Object`, `Array`, `HTMLDivElement`, ...).
    pub class_name: String,
    /// `array`, `regexp`, `date`, `map`, `set`, `error`, `promise`, ...
    /// (`node` is for the host to decide from [`InspectedObject::host`]).
    pub subtype: Option<String>,
    /// Overrides the default description (regexp source, date text, error
    /// stack, proxy).
    pub description: Option<String>,
    /// `length` of arrays and typed arrays, `size` of maps and sets.
    pub length: Option<usize>,
    /// The host object behind a wrapper.
    pub host: Option<HostRef>,
    /// Own enumerable properties (only with a preview), values shallow.
    pub properties: Vec<(String, Inspected)>,
    /// Map/Set entries (only with a preview), `key` is `None` for sets.
    pub entries: Vec<(Option<Inspected>, Inspected)>,
}

/// An exception caught by [`Engine::evaluate_catching`] /
/// [`Engine::call_catching`]: the error and, when JS threw, the thrown
/// value as a handle in the realm.
#[derive(Debug, Clone, PartialEq)]
pub struct Caught {
    pub error: JsError,
    pub value: Option<Handle>,
}

impl From<JsError> for Caught {
    fn from(error: JsError) -> Self {
        Caught { error, value: None }
    }
}

/// State of a value that may be a promise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromiseOutcome {
    /// Not a promise: use the value itself.
    NotAPromise,
    Pending,
    Fulfilled(Handle),
    Rejected(Handle),
}

/// One property for `Runtime.getProperties`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PropertyEntry {
    /// String key (a symbol's description for symbol keys).
    pub name: String,
    /// The key itself when it is a symbol.
    pub symbol: Option<Handle>,
    pub value: Option<Handle>,
    pub get: Option<Handle>,
    pub set: Option<Handle>,
    /// `None` for accessor properties.
    pub writable: Option<bool>,
    pub configurable: bool,
    pub enumerable: bool,
    pub is_own: bool,
}

/// Preview limits, as in V8's inspector (one more than shown, to detect
/// overflow).
const PREVIEW_PROPS: usize = 6;
const PREVIEW_INDEXES: usize = 101;
const PREVIEW_ENTRIES: usize = 6;
/// V8 inspector's limit for `returnByValue` nesting.
const MAX_DEPTH: usize = 1000;

impl Engine {
    /// Describe a held value. With `preview`, own enumerable properties
    /// (and Map/Set entries) are included, shallowly.
    pub fn inspect(
        &mut self,
        realm: RealmId,
        handle: Handle,
        preview: bool,
    ) -> Result<Inspected, JsError> {
        self.run(realm, |scope, state| {
            let v = load_handle(scope, state, handle)?;
            Ok(inspect_value(scope, v, preview))
        })
    }

    /// JSON text of a held value for `returnByValue`: own enumerable
    /// string-keyed properties, recursively; `undefined` properties are
    /// skipped, non-finite numbers become `null`. Cycles and nesting past
    /// 1000 levels fail with V8's "Object reference chain is too long".
    pub fn json_by_value(&mut self, realm: RealmId, handle: Handle) -> Result<String, JsError> {
        self.run(realm, |scope, state| {
            let v = load_handle(scope, state, handle)?;
            let mut out = String::new();
            let mut stack = Vec::new();
            v8::tc_scope!(let tc, scope);
            match to_json(tc, v, &mut stack, &mut out) {
                Ok(()) => Ok(out),
                Err(msg) => {
                    if tc.has_caught() {
                        Err(caught(tc, state))
                    } else {
                        Err(JsError::new(JsErrorKind::Exception, msg))
                    }
                }
            }
        })
    }

    /// Like [`Engine::evaluate_handle`], keeping the thrown value.
    pub fn evaluate_catching(
        &mut self,
        realm: RealmId,
        source: &str,
        origin: ScriptOrigin<'_>,
    ) -> Result<Handle, Caught> {
        let r = self.run(realm, |scope, state| {
            v8::tc_scope!(let tc, scope);
            Ok(match run_script(tc, source, origin) {
                Some(v) => Ok(store_handle(tc, state, realm, v)),
                None => Err(catch_value(tc, state, realm)),
            })
        });
        r.map_err(Caught::from)?
    }

    /// Like [`Engine::call_handle`], keeping the thrown value.
    pub fn call_catching(
        &mut self,
        realm: RealmId,
        func: Handle,
        this: Arg,
        args: &[Arg],
    ) -> Result<Handle, Caught> {
        let r = self.run(realm, |scope, state| {
            v8::tc_scope!(let tc, scope);
            let v = call_inner(tc, state, realm, func, &this, args)?;
            Ok(match v {
                Some(v) => Ok(store_handle(tc, state, realm, v)),
                None => Err(catch_value(tc, state, realm)),
            })
        });
        r.map_err(Caught::from)?
    }

    /// The state of a held value that may be a promise. A pending or
    /// rejected promise is marked as handled.
    pub fn promise_outcome(
        &mut self,
        realm: RealmId,
        handle: Handle,
    ) -> Result<PromiseOutcome, JsError> {
        self.run(realm, |scope, state| {
            let v = load_handle(scope, state, handle)?;
            let Ok(p) = v8::Local::<v8::Promise>::try_from(v) else {
                return Ok(PromiseOutcome::NotAPromise);
            };
            Ok(match p.state() {
                v8::PromiseState::Pending => {
                    p.mark_as_handled();
                    PromiseOutcome::Pending
                }
                v8::PromiseState::Fulfilled => {
                    let r = p.result(scope);
                    PromiseOutcome::Fulfilled(store_handle(scope, state, realm, r))
                }
                v8::PromiseState::Rejected => {
                    p.mark_as_handled();
                    let r = p.result(scope);
                    PromiseOutcome::Rejected(store_handle(scope, state, realm, r))
                }
            })
        })
    }

    /// Properties of a held object: own ones (string and symbol keys, in
    /// order), plus with `own_only == false` those of its prototypes.
    /// `accessors_only` keeps accessor properties only.
    pub fn own_properties(
        &mut self,
        realm: RealmId,
        handle: Handle,
        own_only: bool,
        accessors_only: bool,
    ) -> Result<Vec<PropertyEntry>, JsError> {
        self.run(realm, |scope, state| {
            let v = load_handle(scope, state, handle)?;
            let Ok(mut obj) = v8::Local::<v8::Object>::try_from(v) else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            let mut own = true;
            v8::tc_scope!(let tc, scope);
            loop {
                let args = v8::GetPropertyNamesArgsBuilder::new()
                    .mode(v8::KeyCollectionMode::OwnOnly)
                    .property_filter(v8::PropertyFilter::ALL_PROPERTIES)
                    .index_filter(v8::IndexFilter::IncludeIndices)
                    .key_conversion(v8::KeyConversionMode::ConvertToString)
                    .build();
                if let Some(keys) = obj.get_own_property_names(tc, args) {
                    for i in 0..keys.length() {
                        let Some(k) = keys.get_index(tc, i) else {
                            continue;
                        };
                        let Ok(name) = v8::Local::<v8::Name>::try_from(k) else {
                            continue;
                        };
                        let Some(d) = obj.get_own_property_descriptor(tc, name) else {
                            tc.reset();
                            continue;
                        };
                        let Ok(d) = v8::Local::<v8::Object>::try_from(d) else {
                            continue;
                        };
                        let get = own_field(tc, d, "get");
                        let set = own_field(tc, d, "set");
                        let accessor = get.is_some() || set.is_some();
                        if accessors_only && !accessor {
                            continue;
                        }
                        let value = own_field(tc, d, "value");
                        let writable = own_field(tc, d, "writable").map(|w| w.is_true());
                        let enumerable =
                            own_field(tc, d, "enumerable").is_some_and(|w| w.is_true());
                        let configurable =
                            own_field(tc, d, "configurable").is_some_and(|w| w.is_true());
                        let (name_str, symbol) = if k.is_symbol() {
                            let s = v8::Local::<v8::Symbol>::try_from(k).ok();
                            let desc = s
                                .and_then(|s| s.description(tc).to_string(tc))
                                .map(|d| d.to_rust_string_lossy(tc))
                                .unwrap_or_default();
                            (
                                format!("Symbol({desc})"),
                                Some(store_handle(tc, state, realm, k)),
                            )
                        } else {
                            (k.to_rust_string_lossy(tc), None)
                        };
                        let keep =
                            |tc: &mut v8::PinScope<'_, '_>, v: Option<v8::Local<'_, v8::Value>>| {
                                v.filter(|v| !v.is_undefined() || !accessor)
                                    .map(|v| store_handle(tc, state, realm, v))
                            };
                        out.push(PropertyEntry {
                            name: name_str,
                            symbol,
                            value: if accessor { None } else { keep(tc, value) },
                            get: if accessor { keep(tc, get) } else { None },
                            set: if accessor { keep(tc, set) } else { None },
                            writable: if accessor { None } else { writable },
                            configurable,
                            enumerable,
                            is_own: own,
                        });
                    }
                }
                tc.reset();
                if own_only {
                    break;
                }
                let proto = obj.get_prototype(tc);
                match proto.and_then(|p| v8::Local::<v8::Object>::try_from(p).ok()) {
                    Some(p) => {
                        obj = p;
                        own = false;
                    }
                    None => break,
                }
            }
            Ok(out)
        })
    }

    /// The realm a handle was created in, if it is still alive.
    pub fn handle_realm(&self, handle: Handle) -> Option<RealmId> {
        self.state
            .borrow()
            .handles
            .get(handle.idx, handle.generation)
            .map(|(r, _)| *r)
    }
}

/// `d[name]` if `d` has that own property (descriptor objects).
fn own_field<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    d: v8::Local<'s, v8::Object>,
    name: &str,
) -> Option<v8::Local<'s, v8::Value>> {
    let key = v8_str(scope, name);
    if d.has_own_property(scope, key.into()) != Some(true) {
        return None;
    }
    d.get(scope, key.into())
}

/// The pending exception of `tc` with its value stored as a handle.
fn catch_value(
    tc: &mut v8::PinnedRef<'_, v8::TryCatch<'_, '_, v8::HandleScope<'_>>>,
    state: &SharedState,
    realm: RealmId,
) -> Caught {
    let terminated = tc.has_terminated() || tc.is_execution_terminating();
    let value = if terminated {
        None
    } else {
        tc.exception().map(|e| store_handle(tc, state, realm, e))
    };
    Caught {
        error: caught(tc, state),
        value,
    }
}

fn constructor_name(scope: &mut v8::PinScope<'_, '_>, obj: v8::Local<'_, v8::Object>) -> String {
    let n = obj.get_constructor_name();
    let s = n.to_rust_string_lossy(scope);
    if s.is_empty() { "Object".into() } else { s }
}

fn inspect_value(
    scope: &mut v8::PinScope<'_, '_>,
    v: v8::Local<'_, v8::Value>,
    preview: bool,
) -> Inspected {
    match value_kind(v) {
        ValueKind::Undefined => Inspected::Undefined,
        ValueKind::Null => Inspected::Null,
        ValueKind::Bool => Inspected::Bool(v.is_true()),
        ValueKind::Number => Inspected::Number(v.number_value(scope).unwrap_or(f64::NAN)),
        ValueKind::BigInt => Inspected::BigInt(v.to_rust_string_lossy(scope)),
        ValueKind::String => Inspected::String(v.to_rust_string_lossy(scope)),
        ValueKind::Symbol => {
            let d = detail_string(scope, v);
            let inner = d
                .strip_prefix("Symbol(")
                .and_then(|s| s.strip_suffix(')'))
                .unwrap_or(&d)
                .to_owned();
            Inspected::Symbol(inner)
        }
        ValueKind::Function => {
            let class_name = v8::Local::<v8::Object>::try_from(v)
                .map(|o| constructor_name(scope, o))
                .unwrap_or_else(|_| "Function".into());
            Inspected::Function {
                class_name,
                source: detail_string(scope, v),
            }
        }
        ValueKind::Object => {
            let Ok(obj) = v8::Local::<v8::Object>::try_from(v) else {
                return Inspected::Undefined;
            };
            Inspected::Object(Box::new(inspect_object(scope, obj, preview)))
        }
    }
}

fn inspect_object(
    scope: &mut v8::PinScope<'_, '_>,
    obj: v8::Local<'_, v8::Object>,
    preview: bool,
) -> InspectedObject {
    let v: v8::Local<v8::Value> = obj.into();
    let mut o = InspectedObject {
        class_name: constructor_name(scope, obj),
        host: read_host(scope, obj),
        ..Default::default()
    };
    let sub = |s: &str| Some(s.to_owned());
    if v.is_array() {
        o.subtype = sub("array");
        o.length = v8::Local::<v8::Array>::try_from(v)
            .ok()
            .map(|a| a.length() as usize);
    } else if v.is_reg_exp() {
        o.subtype = sub("regexp");
        o.description = Some(detail_string(scope, v));
    } else if v.is_date() {
        o.subtype = sub("date");
        o.description = Some(detail_string(scope, v));
    } else if v.is_map() {
        o.subtype = sub("map");
        o.length = v8::Local::<v8::Map>::try_from(v).ok().map(|m| m.size());
    } else if v.is_set() {
        o.subtype = sub("set");
        o.length = v8::Local::<v8::Set>::try_from(v).ok().map(|s| s.size());
    } else if v.is_weak_map() {
        o.subtype = sub("weakmap");
    } else if v.is_weak_set() {
        o.subtype = sub("weakset");
    } else if v.is_map_iterator() || v.is_set_iterator() {
        o.subtype = sub("iterator");
    } else if v.is_generator_object() {
        o.subtype = sub("generator");
    } else if v.is_native_error() {
        o.subtype = sub("error");
        v8::tc_scope!(let tc, scope);
        let k = v8_str(tc, "stack");
        let stack = obj
            .get(tc, k.into())
            .filter(|s| s.is_string())
            .map(|s| s.to_rust_string_lossy(tc));
        tc.reset();
        o.description = Some(stack.unwrap_or_else(|| detail_string(tc, v)));
    } else if o.class_name == "DOMException" {
        // Thrown by bindings; described like an error (`name: message`,
        // or the stack when one is attached).
        o.subtype = sub("error");
        v8::tc_scope!(let tc, scope);
        let read = |k: &str| {
            let key = v8_str(tc, k);
            obj.get(tc, key.into())
                .filter(|s| s.is_string())
                .map(|s| s.to_rust_string_lossy(tc))
        };
        let stack = read("stack");
        let name = read("name").unwrap_or_else(|| "Error".into());
        let message = read("message").unwrap_or_default();
        tc.reset();
        o.description = Some(stack.unwrap_or_else(|| {
            if message.is_empty() {
                name
            } else {
                format!("{name}: {message}")
            }
        }));
    } else if v.is_proxy() {
        o.subtype = sub("proxy");
        o.class_name = "Object".into();
        o.description = Some("Proxy(Object)".into());
    } else if v.is_promise() {
        o.subtype = sub("promise");
        o.class_name = "Promise".into();
    } else if v.is_typed_array() {
        o.subtype = sub("typedarray");
        o.length = v8::Local::<v8::TypedArray>::try_from(v)
            .ok()
            .map(|t| t.length());
    } else if v.is_array_buffer() || v.is_shared_array_buffer() {
        o.subtype = sub("arraybuffer");
    } else if v.is_data_view() {
        o.subtype = sub("dataview");
    }
    if !preview || v.is_proxy() {
        return o;
    }
    let is_array = matches!(o.subtype.as_deref(), Some("array" | "typedarray"));
    let max = if is_array {
        PREVIEW_INDEXES
    } else {
        PREVIEW_PROPS
    };
    v8::tc_scope!(let tc, scope);
    let args = v8::GetPropertyNamesArgsBuilder::new()
        .mode(v8::KeyCollectionMode::OwnOnly)
        .property_filter(v8::PropertyFilter::ONLY_ENUMERABLE | v8::PropertyFilter::SKIP_SYMBOLS)
        .key_conversion(v8::KeyConversionMode::ConvertToString)
        .build();
    if let Some(keys) = obj.get_own_property_names(tc, args) {
        for i in 0..keys.length().min(max as u32) {
            let Some(k) = keys.get_index(tc, i) else {
                continue;
            };
            let name = k.to_rust_string_lossy(tc);
            let val = match obj.get(tc, k) {
                Some(val) => shallow(tc, val),
                None => {
                    tc.reset();
                    continue;
                }
            };
            o.properties.push((name, val));
        }
    }
    let pairs = if v.is_map() {
        v8::Local::<v8::Map>::try_from(v)
            .ok()
            .map(|m| (m.as_array(tc), true))
    } else if v.is_set() {
        v8::Local::<v8::Set>::try_from(v)
            .ok()
            .map(|s| (s.as_array(tc), false))
    } else {
        None
    };
    if let Some((arr, keyed)) = pairs {
        let step = if keyed { 2 } else { 1 };
        let n = arr.length().min((PREVIEW_ENTRIES * step) as u32);
        let mut i = 0;
        while i + step as u32 <= n {
            let first = arr.get_index(tc, i).map(|x| shallow(tc, x));
            if keyed {
                let val = arr.get_index(tc, i + 1).map(|x| shallow(tc, x));
                o.entries.push((first, val.unwrap_or(Inspected::Undefined)));
            } else {
                o.entries
                    .push((None, first.unwrap_or(Inspected::Undefined)));
            }
            i += step as u32;
        }
    }
    tc.reset();
    o
}

/// A value inside a preview: objects keep class and subtype only.
fn shallow(scope: &mut v8::PinScope<'_, '_>, v: v8::Local<'_, v8::Value>) -> Inspected {
    inspect_value(scope, v, false)
}

fn push_json_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

const TOO_LONG: &str = "Object reference chain is too long";

/// Serialize `v` as JSON the way V8's inspector does for `returnByValue`.
/// `Err` when a getter threw (the try-catch holds it) or on cycles.
fn to_json(
    scope: &mut v8::PinScope<'_, '_>,
    v: v8::Local<'_, v8::Value>,
    stack: &mut Vec<v8::Global<v8::Object>>,
    out: &mut String,
) -> Result<(), String> {
    match value_kind(v) {
        ValueKind::Undefined | ValueKind::Null | ValueKind::Symbol => out.push_str("null"),
        ValueKind::Bool => out.push_str(if v.is_true() { "true" } else { "false" }),
        ValueKind::Number => {
            let n = v.number_value(scope).unwrap_or(f64::NAN);
            if n.is_finite() {
                out.push_str(&v.to_rust_string_lossy(scope));
            } else {
                out.push_str("null");
            }
        }
        ValueKind::BigInt => return Err("Cannot serialize BigInt value".into()),
        ValueKind::String => push_json_string(out, &v.to_rust_string_lossy(scope)),
        ValueKind::Function => out.push_str("{}"),
        ValueKind::Object => {
            let Ok(obj) = v8::Local::<v8::Object>::try_from(v) else {
                out.push_str("null");
                return Ok(());
            };
            if stack.len() >= MAX_DEPTH || stack.iter().any(|g| v8::Local::new(scope, g) == obj) {
                return Err(TOO_LONG.into());
            }
            stack.push(v8::Global::new(scope, obj));
            let r = object_json(scope, obj, v, stack, out);
            stack.pop();
            r?;
        }
    }
    Ok(())
}

fn object_json(
    scope: &mut v8::PinScope<'_, '_>,
    obj: v8::Local<'_, v8::Object>,
    v: v8::Local<'_, v8::Value>,
    stack: &mut Vec<v8::Global<v8::Object>>,
    out: &mut String,
) -> Result<(), String> {
    if let Ok(arr) = v8::Local::<v8::Array>::try_from(v) {
        out.push('[');
        for i in 0..arr.length() {
            if i > 0 {
                out.push(',');
            }
            let item = arr.get_index(scope, i).ok_or("getter threw")?;
            to_json(scope, item, stack, out)?;
        }
        out.push(']');
        return Ok(());
    }
    let args = v8::GetPropertyNamesArgsBuilder::new()
        .mode(v8::KeyCollectionMode::OwnOnly)
        .property_filter(v8::PropertyFilter::ONLY_ENUMERABLE | v8::PropertyFilter::SKIP_SYMBOLS)
        .key_conversion(v8::KeyConversionMode::ConvertToString)
        .build();
    out.push('{');
    let mut first = true;
    if let Some(keys) = obj.get_own_property_names(scope, args) {
        for i in 0..keys.length() {
            let Some(k) = keys.get_index(scope, i) else {
                continue;
            };
            let val = obj.get(scope, k).ok_or("getter threw")?;
            if val.is_undefined() {
                continue;
            }
            if !first {
                out.push(',');
            }
            first = false;
            push_json_string(out, &k.to_rust_string_lossy(scope));
            out.push(':');
            to_json(scope, val, stack, out)?;
        }
    }
    out.push('}');
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> (Engine, RealmId) {
        let mut e = Engine::new(
            Arc::new(Persona::default_chrome()),
            EngineOptions::default(),
        )
        .expect("engine");
        let r = e.new_realm(RealmOptions::default()).expect("realm");
        (e, r)
    }

    fn eval(e: &mut Engine, r: RealmId, src: &str) -> Handle {
        e.evaluate_catching(r, src, ScriptOrigin::new(""))
            .expect("evaluates")
    }

    #[test]
    fn inspects_like_v8() {
        let (mut e, r) = engine();
        let h = eval(&mut e, r, "[1, 2, 3]");
        match e.inspect(r, h, true).unwrap() {
            Inspected::Object(o) => {
                assert_eq!(o.class_name, "Array");
                assert_eq!(o.subtype.as_deref(), Some("array"));
                assert_eq!(o.length, Some(3));
                assert_eq!(o.properties.len(), 3);
            }
            other => panic!("{other:?}"),
        }
        let h = eval(&mut e, r, "new Map([[1, 'a']])");
        match e.inspect(r, h, true).unwrap() {
            Inspected::Object(o) => {
                assert_eq!(o.subtype.as_deref(), Some("map"));
                assert_eq!(o.length, Some(1));
                assert_eq!(o.entries.len(), 1);
            }
            other => panic!("{other:?}"),
        }
        let h = eval(&mut e, r, "(async function f(a) { return a })");
        match e.inspect(r, h, false).unwrap() {
            Inspected::Function { class_name, source } => {
                assert_eq!(class_name, "AsyncFunction");
                assert_eq!(source, "async function f(a) { return a }");
            }
            other => panic!("{other:?}"),
        }
        let h = eval(&mut e, r, "new TypeError('boom')");
        match e.inspect(r, h, false).unwrap() {
            Inspected::Object(o) => {
                assert_eq!(o.class_name, "TypeError");
                assert!(
                    o.description
                        .unwrap()
                        .starts_with("TypeError: boom\n    at")
                );
            }
            other => panic!("{other:?}"),
        }
        let h = eval(&mut e, r, "Symbol('s')");
        assert_eq!(
            e.inspect(r, h, false).unwrap(),
            Inspected::Symbol("s".into())
        );
    }

    #[test]
    fn by_value_json_and_cycles() {
        let (mut e, r) = engine();
        let h = eval(
            &mut e,
            r,
            "({a: 1, b: 'x\"', c: [true, null, undefined, NaN], d: undefined, e: {f: 0.5}})",
        );
        assert_eq!(
            e.json_by_value(r, h).unwrap(),
            r#"{"a":1,"b":"x\"","c":[true,null,null,null],"e":{"f":0.5}}"#
        );
        let h = eval(&mut e, r, "var o = {}; o.self = o; o");
        assert_eq!(e.json_by_value(r, h).unwrap_err().message, TOO_LONG);
    }

    #[test]
    fn catching_keeps_the_thrown_value_and_promises_settle() {
        let (mut e, r) = engine();
        let c = e
            .evaluate_catching(r, "throw 42", ScriptOrigin::new(""))
            .unwrap_err();
        let v = c.value.expect("value");
        assert_eq!(e.inspect(r, v, false).unwrap(), Inspected::Number(42.0));

        let p = eval(&mut e, r, "Promise.resolve(7)");
        e.perform_microtask_checkpoint();
        match e.promise_outcome(r, p).unwrap() {
            PromiseOutcome::Fulfilled(h) => {
                assert_eq!(e.inspect(r, h, false).unwrap(), Inspected::Number(7.0))
            }
            other => panic!("{other:?}"),
        }
        let p = eval(&mut e, r, "new Promise(() => {})");
        assert_eq!(e.promise_outcome(r, p).unwrap(), PromiseOutcome::Pending);
        let p = eval(&mut e, r, "Promise.reject(new Error('no'))");
        assert!(matches!(
            e.promise_outcome(r, p).unwrap(),
            PromiseOutcome::Rejected(_)
        ));
        let n = eval(&mut e, r, "5");
        assert_eq!(
            e.promise_outcome(r, n).unwrap(),
            PromiseOutcome::NotAPromise
        );
        assert_eq!(e.handle_realm(n), Some(r));
    }

    #[test]
    fn own_properties_with_accessors() {
        let (mut e, r) = engine();
        let h = eval(
            &mut e,
            r,
            "({a: 1, get b() { return 2 }, [Symbol('k')]: 3})",
        );
        let props = e.own_properties(r, h, true, false).unwrap();
        let names: Vec<&str> = props.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "Symbol(k)"]);
        assert!(props[0].value.is_some() && props[0].writable == Some(true));
        assert!(props[1].get.is_some() && props[1].value.is_none());
        assert!(props[2].symbol.is_some());
        let acc = e.own_properties(r, h, true, true).unwrap();
        assert_eq!(acc.len(), 1);
        let all = e.own_properties(r, h, false, false).unwrap();
        assert!(all.iter().any(|p| p.name == "hasOwnProperty" && !p.is_own));
    }
}

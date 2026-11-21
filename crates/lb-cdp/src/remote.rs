//! Building `Runtime.RemoteObject`s the way V8's inspector does.
//!
//! A host describes a JS value as a [`JsValue`] snapshot (it walks the engine
//! value; this crate has no engine). [`remote_object`] turns the snapshot
//! into a `RemoteObject` with V8's `type`/`subtype`/`className`/
//! `description` rules, and with a preview limited like V8's: 5 properties
//! for objects, 100 indexes for arrays, 5 entries for maps and sets, and
//! strings cut at 100 characters.

use serde_json::{Map, Value};

use crate::types::{EntryPreview, ObjectPreview, PropertyPreview, RemoteObject, RemoteObjectId};

/// Properties shown in an object preview.
pub const PREVIEW_MAX_PROPERTIES: usize = 5;
/// Indexes shown in an array preview.
pub const PREVIEW_MAX_INDEXES: usize = 100;
/// Entries shown in a Map/Set preview.
pub const PREVIEW_MAX_ENTRIES: usize = 5;
/// Characters of a string value kept in a preview.
pub const PREVIEW_MAX_STRING: usize = 100;

/// A shallow description of a JS value, filled in by the host.
#[derive(Clone, Debug, PartialEq)]
pub enum JsValue {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    /// Decimal digits, without the `n` suffix.
    BigInt(String),
    String(String),
    /// The symbol's description (`Symbol(desc)`).
    Symbol(String),
    Function(FunctionInfo),
    Object(Box<ObjectInfo>),
}

/// A function value.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FunctionInfo {
    /// `Function`, `AsyncFunction`, `GeneratorFunction`, ...
    pub class_name: String,
    /// `Function.prototype.toString()` text (V8 uses it as description).
    pub source: String,
}

/// An object value. Only `class_name` is required.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ObjectInfo {
    /// Constructor name: `Object`, `Array`, `HTMLDivElement`, `Map`, ...
    pub class_name: String,
    /// `array`, `node`, `regexp`, `date`, `map`, `set`, `error`, `promise`, ...
    pub subtype: Option<String>,
    /// Overrides the computed description (e.g. a node's `div#id.cls`, an
    /// error's stack, a date's `toString()`, a regexp's source).
    pub description: Option<String>,
    /// `length` for arrays and typed arrays, `size` for maps and sets.
    pub length: Option<usize>,
    /// Own enumerable properties in order (indexes first for arrays).
    /// Values may be shallow: nested objects need only class and subtype.
    pub properties: Vec<(String, JsValue)>,
    /// Map/Set entries (`key` is `None` for sets).
    pub entries: Vec<(Option<JsValue>, JsValue)>,
}

impl JsValue {
    pub fn object(class_name: &str) -> JsValue {
        JsValue::Object(Box::new(ObjectInfo {
            class_name: class_name.into(),
            ..Default::default()
        }))
    }

    /// `type` as `typeof` reports it (except `null`, which is an object).
    pub fn type_name(&self) -> &'static str {
        match self {
            JsValue::Undefined => "undefined",
            JsValue::Null | JsValue::Object(_) => "object",
            JsValue::Bool(_) => "boolean",
            JsValue::Number(_) => "number",
            JsValue::BigInt(_) => "bigint",
            JsValue::String(_) => "string",
            JsValue::Symbol(_) => "symbol",
            JsValue::Function(_) => "function",
        }
    }

    fn subtype(&self) -> Option<String> {
        match self {
            JsValue::Null => Some("null".into()),
            JsValue::Object(o) => o.subtype.clone(),
            _ => None,
        }
    }

    /// The `description` V8 would give this value.
    pub fn description(&self) -> Option<String> {
        match self {
            JsValue::Undefined | JsValue::Bool(_) | JsValue::String(_) => None,
            JsValue::Null => None,
            JsValue::Number(n) => Some(js_number_to_string(*n)),
            JsValue::BigInt(d) => Some(format!("{d}n")),
            JsValue::Symbol(d) => Some(format!("Symbol({d})")),
            JsValue::Function(f) => Some(f.source.clone()),
            JsValue::Object(o) => Some(object_description(o)),
        }
    }

    /// The JSON value for `returnByValue`. Functions and symbols become
    /// `{}` / `null`; numbers that JSON cannot hold become `null`.
    pub fn to_json(&self) -> Value {
        match self {
            JsValue::Undefined | JsValue::Null | JsValue::Symbol(_) => Value::Null,
            JsValue::Bool(b) => Value::Bool(*b),
            JsValue::Number(n) => serde_json::Number::from_f64(*n)
                .map(|v| {
                    if n.fract() == 0.0 && n.abs() < 9.0e15 {
                        Value::from(*n as i64)
                    } else {
                        Value::Number(v)
                    }
                })
                .unwrap_or(Value::Null),
            JsValue::BigInt(_) => Value::Null,
            JsValue::String(s) => Value::String(s.clone()),
            JsValue::Function(_) => Value::Object(Map::new()),
            JsValue::Object(o) => {
                if o.subtype.as_deref() == Some("array") {
                    Value::Array(o.properties.iter().map(|(_, v)| v.to_json()).collect())
                } else {
                    let mut m = Map::new();
                    for (k, v) in &o.properties {
                        if !matches!(v, JsValue::Undefined | JsValue::Function(_)) {
                            m.insert(k.clone(), v.to_json());
                        }
                    }
                    Value::Object(m)
                }
            }
        }
    }
}

fn object_description(o: &ObjectInfo) -> String {
    if let Some(d) = &o.description {
        return d.clone();
    }
    match o.subtype.as_deref() {
        Some("array") => format!(
            "{}({})",
            o.class_name,
            o.length.unwrap_or(o.properties.len())
        ),
        Some("typedarray") => format!("{}({})", o.class_name, o.length.unwrap_or(0)),
        Some("map") | Some("set") | Some("weakmap") | Some("weakset") => match o.length {
            Some(n) if !o.subtype.as_deref().unwrap_or("").starts_with("weak") => {
                format!("{}({n})", o.class_name)
            }
            _ => o.class_name.clone(),
        },
        _ => o.class_name.clone(),
    }
}

/// JS `Number.prototype.toString()` for the common cases.
pub fn js_number_to_string(n: f64) -> String {
    if n.is_nan() {
        return "NaN".into();
    }
    if n.is_infinite() {
        return if n > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    if n == 0.0 {
        return if n.is_sign_negative() { "-0" } else { "0" }.into();
    }
    let abs = n.abs();
    if !(1e-6..1e21).contains(&abs) {
        // Rust prints `1e21` / `1.5e-7`; JS wants `1e+21` / `1.5e-7`.
        let s = format!("{n:e}");
        return match s.split_once('e') {
            Some((m, e)) if !e.starts_with('-') => format!("{m}e+{e}"),
            _ => s,
        };
    }
    if n.fract() == 0.0 && abs < 9.0e15 {
        return format!("{}", n as i64);
    }
    format!("{n}")
}

/// Unserializable numbers (`NaN`, `Infinity`, `-Infinity`, `-0`).
fn unserializable(n: f64) -> Option<String> {
    if n.is_nan() || n.is_infinite() || (n == 0.0 && n.is_sign_negative()) {
        Some(js_number_to_string(n))
    } else {
        None
    }
}

/// How to build the remote object.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RemoteOptions {
    /// Put the value itself in `value` (objects as JSON) and no object id.
    pub return_by_value: bool,
    /// Attach an `ObjectPreview` to objects.
    pub generate_preview: bool,
}

/// Builds the `RemoteObject` for `value`. `object_id` is the host's handle
/// for objects, functions and symbols (ignored with `return_by_value`).
pub fn remote_object(
    value: &JsValue,
    object_id: Option<RemoteObjectId>,
    opts: RemoteOptions,
) -> RemoteObject {
    let mut r = RemoteObject {
        kind: value.type_name().into(),
        subtype: value.subtype(),
        ..Default::default()
    };
    match value {
        JsValue::Undefined => {}
        JsValue::Null => r.value = Some(Value::Null),
        JsValue::Bool(b) => r.value = Some(Value::Bool(*b)),
        JsValue::String(s) => r.value = Some(Value::String(s.clone())),
        JsValue::Number(n) => {
            match unserializable(*n) {
                Some(u) => r.unserializable_value = Some(u),
                None => r.value = Some(value.to_json()),
            }
            r.description = value.description();
        }
        JsValue::BigInt(d) => {
            r.unserializable_value = Some(format!("{d}n"));
            r.description = value.description();
        }
        JsValue::Symbol(_) | JsValue::Function(_) | JsValue::Object(_) => {
            r.description = value.description();
            r.class_name = match value {
                JsValue::Function(f) => Some(if f.class_name.is_empty() {
                    "Function".into()
                } else {
                    f.class_name.clone()
                }),
                JsValue::Object(o) => Some(o.class_name.clone()),
                _ => None,
            };
            if opts.return_by_value {
                r.value = Some(value.to_json());
                if matches!(value, JsValue::Symbol(_)) {
                    r.value = None;
                    r.object_id = object_id;
                }
            } else {
                r.object_id = object_id;
                if opts.generate_preview
                    && let JsValue::Object(o) = value
                {
                    r.preview = Some(object_preview(o));
                }
            }
        }
    }
    r
}

/// The preview of an object, cut to V8's limits.
pub fn object_preview(o: &ObjectInfo) -> ObjectPreview {
    let is_array = matches!(o.subtype.as_deref(), Some("array" | "typedarray"));
    let max = if is_array {
        PREVIEW_MAX_INDEXES
    } else {
        PREVIEW_MAX_PROPERTIES
    };
    let properties = o
        .properties
        .iter()
        .take(max)
        .map(|(name, v)| property_preview(name, v))
        .collect();
    let mut overflow = o.properties.len() > max;
    let entries = if matches!(
        o.subtype.as_deref(),
        Some("map" | "set" | "weakmap" | "weakset")
    ) {
        overflow |= o.entries.len() > PREVIEW_MAX_ENTRIES;
        Some(
            o.entries
                .iter()
                .take(PREVIEW_MAX_ENTRIES)
                .map(|(k, v)| EntryPreview {
                    key: k.as_ref().map(shallow_preview),
                    value: shallow_preview(v),
                })
                .collect(),
        )
    } else {
        None
    };
    ObjectPreview {
        kind: "object".into(),
        subtype: o.subtype.clone(),
        description: Some(object_description(o)),
        overflow,
        properties,
        entries,
    }
}

/// A one-level preview used for entries and nested values.
fn shallow_preview(v: &JsValue) -> ObjectPreview {
    ObjectPreview {
        kind: v.type_name().into(),
        subtype: v.subtype(),
        description: Some(abbreviated(v)),
        overflow: false,
        properties: Vec::new(),
        entries: None,
    }
}

fn property_preview(name: &str, v: &JsValue) -> PropertyPreview {
    PropertyPreview {
        name: name.into(),
        kind: v.type_name().into(),
        value: Some(abbreviated(v)),
        value_preview: None,
        subtype: v.subtype(),
    }
}

/// Short text for a value inside a preview.
fn abbreviated(v: &JsValue) -> String {
    match v {
        JsValue::Undefined => "undefined".into(),
        JsValue::Null => "null".into(),
        JsValue::Bool(b) => b.to_string(),
        JsValue::String(s) => {
            if s.chars().count() > PREVIEW_MAX_STRING {
                let cut: String = s.chars().take(PREVIEW_MAX_STRING - 1).collect();
                format!("{cut}…")
            } else {
                s.clone()
            }
        }
        JsValue::Function(_) => String::new(),
        _ => v.description().unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ro(v: &JsValue) -> Value {
        serde_json::to_value(remote_object(
            v,
            Some("1.1.1".into()),
            RemoteOptions::default(),
        ))
        .unwrap()
    }

    #[test]
    fn primitives_match_v8() {
        assert_eq!(ro(&JsValue::Undefined), json!({"type": "undefined"}));
        assert_eq!(
            ro(&JsValue::Null),
            json!({"type": "object", "subtype": "null", "value": null})
        );
        assert_eq!(
            ro(&JsValue::Number(2.0)),
            json!({"type": "number", "value": 2, "description": "2"})
        );
        assert_eq!(
            ro(&JsValue::Number(0.5)),
            json!({"type": "number", "value": 0.5, "description": "0.5"})
        );
        assert_eq!(
            ro(&JsValue::Number(-0.0)),
            json!({"type": "number", "unserializableValue": "-0", "description": "-0"})
        );
        assert_eq!(
            ro(&JsValue::Number(f64::NAN)),
            json!({"type": "number", "unserializableValue": "NaN", "description": "NaN"})
        );
        assert_eq!(
            ro(&JsValue::BigInt("12".into())),
            json!({"type": "bigint", "unserializableValue": "12n", "description": "12n"})
        );
        assert_eq!(
            ro(&JsValue::String("hi".into())),
            json!({"type": "string", "value": "hi"})
        );
        assert_eq!(js_number_to_string(1e21), "1e+21");
        assert_eq!(js_number_to_string(1.5e-7), "1.5e-7");
        assert_eq!(js_number_to_string(123456.0), "123456");
    }

    #[test]
    fn objects_get_descriptions_and_limited_previews() {
        let arr = JsValue::Object(Box::new(ObjectInfo {
            class_name: "Array".into(),
            subtype: Some("array".into()),
            length: Some(3),
            properties: (0..3)
                .map(|i| (i.to_string(), JsValue::Number(i as f64)))
                .collect(),
            ..Default::default()
        }));
        let r = ro(&arr);
        assert_eq!(r["description"], "Array(3)");
        assert_eq!(r["className"], "Array");
        assert_eq!(r["objectId"], "1.1.1");
        assert!(r.get("preview").is_none());

        let obj = JsValue::Object(Box::new(ObjectInfo {
            class_name: "Object".into(),
            properties: (0..7)
                .map(|i| (format!("k{i}"), JsValue::String("x".repeat(150))))
                .collect(),
            ..Default::default()
        }));
        let r = remote_object(
            &obj,
            Some("o".into()),
            RemoteOptions {
                generate_preview: true,
                return_by_value: false,
            },
        );
        let p = r.preview.unwrap();
        assert!(p.overflow);
        assert_eq!(p.properties.len(), PREVIEW_MAX_PROPERTIES);
        assert_eq!(
            p.properties[0].value.as_ref().unwrap().chars().count(),
            PREVIEW_MAX_STRING
        );

        let by_value = remote_object(
            &obj,
            Some("o".into()),
            RemoteOptions {
                return_by_value: true,
                generate_preview: true,
            },
        );
        assert!(by_value.object_id.is_none());
        assert_eq!(by_value.value.unwrap()["k0"].as_str().unwrap().len(), 150);

        let map = JsValue::Object(Box::new(ObjectInfo {
            class_name: "Map".into(),
            subtype: Some("map".into()),
            length: Some(1),
            entries: vec![(Some(JsValue::String("a".into())), JsValue::Number(1.0))],
            ..Default::default()
        }));
        let r = remote_object(
            &map,
            Some("m".into()),
            RemoteOptions {
                generate_preview: true,
                ..Default::default()
            },
        );
        assert_eq!(r.description.as_deref(), Some("Map(1)"));
        assert_eq!(r.preview.unwrap().entries.unwrap().len(), 1);
    }
}

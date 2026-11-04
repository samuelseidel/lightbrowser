//! IndexedDB keys, key ranges and key paths (ADR 0028): conversion from and
//! to JS values with Chrome's rules (`ScriptValueToIDBKey`), the key order
//! (number < date < string < binary < array) and key path evaluation and
//! injection on structured clones.

use std::cmp::Ordering;

use lb_js::{Arg, CallCtx, Handle, HostError, JsValue, ValueKind};

/// A valid key.
#[derive(Clone, Debug)]
pub enum Key {
    Number(f64),
    Date(f64),
    /// UTF-16 code units (lone surrogates are valid key content).
    String(Vec<u16>),
    Binary(Vec<u8>),
    Array(Vec<Key>),
}

impl Key {
    fn rank(&self) -> u8 {
        match self {
            Key::Number(_) => 0,
            Key::Date(_) => 1,
            Key::String(_) => 2,
            Key::Binary(_) => 3,
            Key::Array(_) => 4,
        }
    }

    /// The JS value of the key (a new object for dates, arrays and binary).
    pub fn to_arg(&self) -> Arg {
        match self {
            Key::Number(n) => Arg::Number(*n),
            Key::Date(t) => Arg::Date(*t),
            Key::String(s) => Arg::Utf16(s.clone()),
            Key::Binary(b) => Arg::ArrayBuffer(b.clone()),
            Key::Array(a) => Arg::Array(a.iter().map(Key::to_arg).collect()),
        }
    }

    pub fn from_str(s: &str) -> Key {
        Key::String(s.encode_utf16().collect())
    }
}

impl Ord for Key {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Key::Number(a), Key::Number(b)) | (Key::Date(a), Key::Date(b)) => {
                a.partial_cmp(b).unwrap_or(Ordering::Equal)
            }
            (Key::String(a), Key::String(b)) => a.cmp(b),
            (Key::Binary(a), Key::Binary(b)) => a.cmp(b),
            (Key::Array(a), Key::Array(b)) => {
                for (x, y) in a.iter().zip(b) {
                    match x.cmp(y) {
                        Ordering::Equal => {}
                        o => return o,
                    }
                }
                a.len().cmp(&b.len())
            }
            _ => self.rank().cmp(&other.rank()),
        }
    }
}

impl PartialOrd for Key {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Key {}

/// A key range (`IDBKeyRange`).
#[derive(Clone, Debug, PartialEq)]
pub struct Range {
    pub lower: Option<Key>,
    pub upper: Option<Key>,
    pub lower_open: bool,
    pub upper_open: bool,
}

impl Range {
    pub fn only(k: Key) -> Range {
        Range {
            lower: Some(k.clone()),
            upper: Some(k),
            lower_open: false,
            upper_open: false,
        }
    }

    pub fn all() -> Range {
        Range {
            lower: None,
            upper: None,
            lower_open: false,
            upper_open: false,
        }
    }

    pub fn contains(&self, k: &Key) -> bool {
        if let Some(l) = &self.lower {
            match k.cmp(l) {
                Ordering::Less => return false,
                Ordering::Equal if self.lower_open => return false,
                _ => {}
            }
        }
        if let Some(u) = &self.upper {
            match k.cmp(u) {
                Ordering::Greater => return false,
                Ordering::Equal if self.upper_open => return false,
                _ => {}
            }
        }
        true
    }
}

/// Convert a JS value to a key; `Ok(None)` when it is not a valid key.
/// Array elements are read with `[[Get]]` (getters run), as in Chrome.
pub fn to_key(cx: &mut CallCtx<'_, '_, '_>, h: Handle) -> Result<Option<Key>, HostError> {
    let mut seen = Vec::new();
    to_key_inner(cx, h, &mut seen)
}

fn to_key_inner(
    cx: &mut CallCtx<'_, '_, '_>,
    h: Handle,
    seen: &mut Vec<Handle>,
) -> Result<Option<Key>, HostError> {
    match cx.handle_kind(h) {
        ValueKind::Number => match cx.handle_value(h) {
            JsValue::Number(n) if !n.is_nan() => Ok(Some(Key::Number(n))),
            _ => Ok(None),
        },
        ValueKind::String => Ok(cx.handle_utf16(h).map(Key::String)),
        ValueKind::Object => {
            if let Some(t) = cx.handle_date(h) {
                return Ok((!t.is_nan()).then_some(Key::Date(t)));
            }
            if cx.is_array_buffer(h) || cx.handle_view_kind(h).is_some() {
                return Ok(cx.handle_bytes(h).map(Key::Binary));
            }
            if !cx.handle_is_array(h) {
                return Ok(None);
            }
            if seen.iter().any(|s| cx.strict_equals(*s, h)) {
                return Ok(None);
            }
            seen.push(h);
            let lh = cx.get_property(h, "length")?;
            let len = match cx.handle_value(lh) {
                JsValue::Number(n) => n as usize,
                _ => 0,
            };
            cx.release(lh);
            let mut out = Vec::with_capacity(len.min(1 << 16));
            for i in 0..len {
                let k = i.to_string();
                if !cx.has_own_property(h, &k)? {
                    seen.pop();
                    return Ok(None);
                }
                let e = cx.get_property(h, &k)?;
                let r = to_key_inner(cx, e, seen);
                cx.release(e);
                match r? {
                    Some(k) => out.push(k),
                    None => {
                        seen.pop();
                        return Ok(None);
                    }
                }
            }
            seen.pop();
            Ok(Some(Key::Array(out)))
        }
        _ => Ok(None),
    }
}

/// A key path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyPath {
    String(String),
    Array(Vec<String>),
}

impl KeyPath {
    pub fn is_valid(&self) -> bool {
        match self {
            KeyPath::String(s) => valid_path(s),
            KeyPath::Array(a) => !a.is_empty() && a.iter().all(|s| valid_path(s)),
        }
    }

    pub fn to_arg(&self) -> Arg {
        match self {
            KeyPath::String(s) => Arg::String(s.clone()),
            KeyPath::Array(a) => Arg::Array(a.iter().map(|s| Arg::String(s.clone())).collect()),
        }
    }

    pub fn is_array(&self) -> bool {
        matches!(self, KeyPath::Array(_))
    }

    pub fn is_empty_string(&self) -> bool {
        matches!(self, KeyPath::String(s) if s.is_empty())
    }
}

fn id_start(c: char) -> bool {
    c == '$' || c == '_' || c.is_alphabetic()
}

fn id_part(c: char) -> bool {
    id_start(c) || c.is_alphanumeric() || c == '\u{200c}' || c == '\u{200d}'
}

/// A valid key path string: empty, or identifiers joined by `.`.
pub fn valid_path(s: &str) -> bool {
    if s.is_empty() {
        return true;
    }
    s.split('.').all(|id| {
        let mut cs = id.chars();
        cs.next().is_some_and(id_start) && cs.all(id_part)
    })
}

/// A key path from a JS value (`keyPath` option or `createIndex`
/// argument): `None` for `undefined`/`null` when `nullable`, an array of
/// strings, or the value's string. Validity is checked by the caller.
pub fn key_path_arg(
    cx: &mut CallCtx<'_, '_, '_>,
    h: Handle,
    nullable: bool,
) -> Result<Option<KeyPath>, HostError> {
    match cx.handle_kind(h) {
        ValueKind::Undefined | ValueKind::Null if nullable => Ok(None),
        ValueKind::Object if cx.handle_is_array(h) => {
            let lh = cx.get_property(h, "length")?;
            let len = match cx.handle_value(lh) {
                JsValue::Number(n) => n as usize,
                _ => 0,
            };
            cx.release(lh);
            let mut out = Vec::new();
            for i in 0..len {
                let e = cx.get_property(h, &i.to_string())?;
                let s = cx.handle_string(e);
                cx.release(e);
                out.push(s?);
            }
            Ok(Some(KeyPath::Array(out)))
        }
        _ => Ok(Some(KeyPath::String(cx.handle_string(h)?))),
    }
}

/// Evaluate one key path string on a value: the value it names, or `None`
/// (failure).
fn eval_string(
    cx: &mut CallCtx<'_, '_, '_>,
    value: Handle,
    path: &str,
) -> Result<Option<Handle>, HostError> {
    let mut cur = cx.make_handle(Arg::Handle(value))?;
    if path.is_empty() {
        return Ok(Some(cur));
    }
    for id in path.split('.') {
        let kind = cx.handle_kind(cur);
        if id == "length" && kind == ValueKind::String {
            let n = cx.handle_utf16(cur).map_or(0, |u| u.len());
            cx.release(cur);
            cur = cx.make_handle(Arg::Number(n as f64))?;
            continue;
        }
        if kind != ValueKind::Object && kind != ValueKind::Function {
            cx.release(cur);
            return Ok(None);
        }
        if !cx.has_own_property(cur, id)? {
            cx.release(cur);
            return Ok(None);
        }
        let next = cx.get_property(cur, id)?;
        cx.release(cur);
        cur = next;
    }
    Ok(Some(cur))
}

/// Evaluate a key path on a value and convert the result to a key.
/// `Ok(Err(()))`: the path did not yield a value; `Ok(Ok(None))`: the
/// value is not a valid key.
pub fn eval_key(
    cx: &mut CallCtx<'_, '_, '_>,
    value: Handle,
    path: &KeyPath,
) -> Result<Result<Option<Key>, ()>, HostError> {
    match path {
        KeyPath::String(p) => {
            let Some(v) = eval_string(cx, value, p)? else {
                return Ok(Err(()));
            };
            let k = to_key(cx, v);
            cx.release(v);
            Ok(Ok(k?))
        }
        KeyPath::Array(ps) => {
            let mut out = Vec::new();
            for p in ps {
                let Some(v) = eval_string(cx, value, p)? else {
                    return Ok(Err(()));
                };
                let k = to_key(cx, v);
                cx.release(v);
                match k? {
                    Some(k) => out.push(k),
                    None => return Ok(Ok(None)),
                }
            }
            Ok(Ok(Some(Key::Array(out))))
        }
    }
}

/// Index keys of a value for an index with `path` and `multi_entry`
/// (Chrome: values that are not valid keys give no entry; a multiEntry
/// array gives its valid, distinct elements).
pub fn index_keys(
    cx: &mut CallCtx<'_, '_, '_>,
    value: Handle,
    path: &KeyPath,
    multi_entry: bool,
) -> Result<Vec<Key>, HostError> {
    if !multi_entry || path.is_array() {
        return Ok(match eval_key(cx, value, path)? {
            Ok(Some(k)) => vec![k],
            _ => Vec::new(),
        });
    }
    let KeyPath::String(p) = path else {
        return Ok(Vec::new());
    };
    let Some(v) = eval_string(cx, value, p)? else {
        return Ok(Vec::new());
    };
    let mut out: Vec<Key> = Vec::new();
    if cx.handle_kind(v) == ValueKind::Object && cx.handle_is_array(v) {
        let lh = cx.get_property(v, "length")?;
        let len = match cx.handle_value(lh) {
            JsValue::Number(n) => n as usize,
            _ => 0,
        };
        cx.release(lh);
        for i in 0..len {
            let e = cx.get_property(v, &i.to_string())?;
            let k = to_key(cx, e);
            cx.release(e);
            if let Some(k) = k?
                && !out.contains(&k)
            {
                out.push(k);
            }
        }
    } else if let Some(k) = to_key(cx, v)? {
        out.push(k);
    }
    cx.release(v);
    out.sort();
    Ok(out)
}

/// Whether a generated key could be injected at `path` into `value`.
pub fn can_inject(
    cx: &mut CallCtx<'_, '_, '_>,
    value: Handle,
    path: &str,
) -> Result<bool, HostError> {
    let ids: Vec<&str> = path.split('.').collect();
    let mut cur = cx.make_handle(Arg::Handle(value))?;
    for id in &ids[..ids.len() - 1] {
        if cx.handle_kind(cur) != ValueKind::Object {
            cx.release(cur);
            return Ok(false);
        }
        if !cx.has_own_property(cur, id)? {
            cx.release(cur);
            return Ok(true);
        }
        let next = cx.get_property(cur, id)?;
        cx.release(cur);
        cur = next;
    }
    let ok = cx.handle_kind(cur) == ValueKind::Object;
    cx.release(cur);
    Ok(ok)
}

/// Inject `key` at `path` into `value` (creating intermediate objects).
pub fn inject(js: &mut dyn super::super::Js, value: Handle, path: &str, key: &Key) -> bool {
    let ids: Vec<&str> = path.split('.').collect();
    let mut cur = value;
    let mut owned: Vec<Handle> = Vec::new();
    let ok = (|| {
        for id in &ids[..ids.len() - 1] {
            if js.kind(cur) != ValueKind::Object {
                return false;
            }
            let next = match js.get(cur, id) {
                Some(n) => n,
                None => return false,
            };
            if js.kind(next) == ValueKind::Undefined {
                js.release(next);
                let Some(obj) = js.make(Arg::Json("{}".into())) else {
                    return false;
                };
                if !set(js, cur, id, Arg::Handle(obj)) {
                    return false;
                }
                owned.push(obj);
                cur = obj;
            } else {
                owned.push(next);
                cur = next;
            }
        }
        if js.kind(cur) != ValueKind::Object {
            return false;
        }
        set(js, cur, ids[ids.len() - 1], key.to_arg())
    })();
    for h in owned {
        js.release(h);
    }
    ok
}

fn set(js: &mut dyn super::super::Js, obj: Handle, name: &str, v: Arg) -> bool {
    js.define(obj, name, v)
}

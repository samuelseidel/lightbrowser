//! Console arguments as Chrome handles them, with no DevTools attached.
//!
//! Chrome runs page code during a console call in exactly two places, and
//! nowhere else:
//!
//! 1. V8's `Formatter` (`src/builtins/builtins-console.cc`): for `log`,
//!    `debug`, `info`, `warn`, `error`, `trace`, `group`, `groupCollapsed`
//!    and `assert`, a leading string's `%s` runs `String(arg)`, `%d`/`%i`
//!    `parseInt(arg, 10)` and `%f` `parseFloat(arg)`. A throw reaches the
//!    page.
//! 2. The inspector's `V8ValueStringBuilder` (`src/inspector/
//!    v8-console-message.cc`), which turns every argument into the stored
//!    message text: `Object.prototype.toString` for ordinary objects (so a
//!    `Symbol.toStringTag` getter runs), `ToString` for dates, functions,
//!    native errors and regexps, array elements one by one; proxies are
//!    `[object Proxy]`. Throws are swallowed.
//!
//! Plain getters, `toJSON`, an object's own `toString` and proxy traps never
//! run (recorded in Chrome 153 with `--dump-dom`, no CDP client;
//! `lb-page` `tests/console/`). [`CallCtx::console_args`] does the same
//! steps, then builds the text we report from data properties only, so
//! reading it runs nothing.

use super::*;

/// V8's own functions the string builder calls, taken from a realm before
/// any page script ran, so a page that replaces them changes nothing.
pub(super) struct ConsoleBuiltins {
    object_to_string: v8::Global<v8::Function>,
    string_value_of: v8::Global<v8::Function>,
    number_value_of: v8::Global<v8::Function>,
    boolean_value_of: v8::Global<v8::Function>,
    symbol_value_of: v8::Global<v8::Function>,
    bigint_value_of: v8::Global<v8::Function>,
}

/// Read `global[ctor].prototype[name]` from a fresh realm (entered).
fn proto_fn(
    scope: &mut v8::PinScope<'_, '_>,
    global: v8::Local<'_, v8::Object>,
    ctor: &str,
    name: &str,
) -> Option<v8::Global<v8::Function>> {
    let k = v8_str(scope, ctor);
    let c = v8::Local::<v8::Object>::try_from(global.get(scope, k.into())?).ok()?;
    let k = v8_str(scope, "prototype");
    let p = v8::Local::<v8::Object>::try_from(c.get(scope, k.into())?).ok()?;
    let k = v8_str(scope, name);
    let f = v8::Local::<v8::Function>::try_from(p.get(scope, k.into())?).ok()?;
    Some(v8::Global::new(scope, f))
}

pub(super) fn capture_builtins(
    scope: &mut v8::PinScope<'_, '_>,
    global: v8::Local<'_, v8::Object>,
) -> Option<ConsoleBuiltins> {
    Some(ConsoleBuiltins {
        object_to_string: proto_fn(scope, global, "Object", "toString")?,
        string_value_of: proto_fn(scope, global, "String", "valueOf")?,
        number_value_of: proto_fn(scope, global, "Number", "valueOf")?,
        boolean_value_of: proto_fn(scope, global, "Boolean", "valueOf")?,
        symbol_value_of: proto_fn(scope, global, "Symbol", "valueOf")?,
        bigint_value_of: proto_fn(scope, global, "BigInt", "valueOf")?,
    })
}

/// The builtins as locals of the current scope.
struct Fns<'s> {
    object_to_string: v8::Local<'s, v8::Function>,
    string_value_of: v8::Local<'s, v8::Function>,
    number_value_of: v8::Local<'s, v8::Function>,
    boolean_value_of: v8::Local<'s, v8::Function>,
    symbol_value_of: v8::Local<'s, v8::Function>,
    bigint_value_of: v8::Local<'s, v8::Function>,
}

impl<'s, 'i> CallCtx<'_, 's, 'i> {
    /// A console method's arguments from `start` on, handled as Chrome does
    /// (see the module docs): `format` runs V8's `Formatter` first, then each
    /// argument goes through the inspector's string builder. Returns the text
    /// of each argument for our console message: JSON of own data properties
    /// for plain objects and arrays, the string builder's text otherwise.
    /// `Err(HostError::Thrown)` when a format conversion threw (it stays
    /// pending for the page) or execution is terminating.
    pub fn console_args(&mut self, start: usize, format: bool) -> Result<Vec<String>, HostError> {
        let fns = {
            let st = self.state.borrow();
            let Some(b) = st.realms.get(&self.realm).and_then(|r| r.console.as_ref()) else {
                return Err(HostError::type_error("Illegal invocation"));
            };
            Fns {
                object_to_string: v8::Local::new(self.scope, &b.object_to_string),
                string_value_of: v8::Local::new(self.scope, &b.string_value_of),
                number_value_of: v8::Local::new(self.scope, &b.number_value_of),
                boolean_value_of: v8::Local::new(self.scope, &b.boolean_value_of),
                symbol_value_of: v8::Local::new(self.scope, &b.symbol_value_of),
                bigint_value_of: v8::Local::new(self.scope, &b.bigint_value_of),
            }
        };
        let mut args: Vec<v8::Local<'s, v8::Value>> =
            (start..self.len()).map(|i| self.arg(i)).collect();
        if format {
            formatter(self.scope, &mut args)?;
        }
        let mut out = Vec::with_capacity(args.len());
        for &v in &args {
            let text = builder_text(self.scope, &fns, v)?;
            out.push(if is_plain_data(v) {
                data_json(self.scope, v).unwrap_or(text)
            } else {
                text
            });
        }
        Ok(out)
    }

    /// The inspector's `firstArgToString` for the label of `count`, `time`,
    /// `timeEnd`, `timeLog`, `timeStamp`, `profile`, ...: a string as is,
    /// `"default"` when missing or undefined, else `ToString` (which may run
    /// page code and throw into the page).
    pub fn console_label(&mut self, i: usize) -> Result<String, HostError> {
        if i >= self.len() {
            return Ok("default".into());
        }
        let v = self.arg(i);
        if v.is_undefined() {
            return Ok("default".into());
        }
        let s = v.to_string(self.scope).ok_or(HostError::Thrown)?;
        Ok(s.to_rust_string_lossy(self.scope))
    }
}

// ---------------------------------------------------------------------------
// Formatter
// ---------------------------------------------------------------------------

/// V8's `Formatter`: converts the arguments the format specifiers of a
/// leading string consume, in place. `%s` results are scanned again.
fn formatter<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: &mut [v8::Local<'s, v8::Value>],
) -> Result<(), HostError> {
    if args.len() < 2 || !args[0].is_string() {
        return Ok(());
    }
    let utf16 = |scope: &mut v8::PinScope<'s, '_>, v: v8::Local<'s, v8::Value>| -> Vec<u16> {
        v.to_rust_string_lossy(scope).encode_utf16().collect()
    };
    let mut states: Vec<(Vec<u16>, usize)> = vec![(utf16(scope, args[0]), 0)];
    let mut index = 1;
    while index < args.len() {
        let Some(top) = states.last_mut() else { break };
        let Some(pos) = top.0[top.1.min(top.0.len())..]
            .iter()
            .position(|&c| c == u16::from(b'%'))
            .map(|p| p + top.1)
        else {
            states.pop();
            continue;
        };
        if pos == top.0.len() - 1 {
            states.pop();
            continue;
        }
        let spec = top.0[pos + 1];
        let current = args[index];
        let converted: v8::Local<'s, v8::Value> = match char::from_u32(u32::from(spec)) {
            Some(c @ ('d' | 'f' | 'i')) => {
                top.1 = pos + 2;
                let n = if current.is_symbol() {
                    f64::NAN
                } else {
                    let s = current.to_string(scope).ok_or(HostError::Thrown)?;
                    let s = s.to_rust_string_lossy(scope);
                    if c == 'f' {
                        parse_float(&s)
                    } else {
                        parse_int(&s)
                    }
                };
                v8::Number::new(scope, n).into()
            }
            Some('s') => {
                top.1 = pos + 2;
                let s: v8::Local<'s, v8::Value> =
                    if let Ok(sym) = v8::Local::<v8::Symbol>::try_from(current) {
                        // `String(symbol)` is its descriptive string; no throw.
                        let d = sym.description(scope);
                        let d = if d.is_undefined() {
                            String::new()
                        } else {
                            d.to_rust_string_lossy(scope)
                        };
                        v8_str(scope, &format!("Symbol({d})")).into()
                    } else {
                        current.to_string(scope).ok_or(HostError::Thrown)?.into()
                    };
                let scan = utf16(scope, s);
                states.push((scan, 0));
                s
            }
            Some('c' | 'o' | 'O' | '_') => {
                top.1 = pos + 2;
                index += 1;
                continue;
            }
            Some('%') => {
                top.1 = pos + 2;
                continue;
            }
            _ => {
                top.1 = pos + 1;
                continue;
            }
        };
        args[index] = converted;
        index += 1;
    }
    Ok(())
}

fn is_js_space(c: char) -> bool {
    c.is_whitespace() || c == '\u{feff}'
}

/// `parseInt(s, 10)` of an already converted string.
fn parse_int(s: &str) -> f64 {
    let t = s.trim_start_matches(is_js_space);
    let (neg, t) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let digits: &str = &t[..t.bytes().take_while(u8::is_ascii_digit).count()];
    if digits.is_empty() {
        return f64::NAN;
    }
    let n: f64 = digits.parse().unwrap_or(f64::NAN);
    if neg { -n } else { n }
}

/// `parseFloat(s)` of an already converted string.
fn parse_float(s: &str) -> f64 {
    let t = s.trim_start_matches(is_js_space);
    let b = t.as_bytes();
    let mut i = 0;
    if matches!(b.first(), Some(b'+' | b'-')) {
        i = 1;
    }
    if t[i..].starts_with("Infinity") {
        return if b.first() == Some(&b'-') {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        };
    }
    let int = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
    let mut end = i + int;
    let mut frac = 0;
    if b.get(end) == Some(&b'.') {
        frac = b[end + 1..]
            .iter()
            .take_while(|c| c.is_ascii_digit())
            .count();
        if int + frac > 0 {
            end += 1 + frac;
        }
    }
    if int + frac == 0 {
        return f64::NAN;
    }
    if matches!(b.get(end), Some(b'e' | b'E')) {
        let mut j = end + 1;
        if matches!(b.get(j), Some(b'+' | b'-')) {
            j += 1;
        }
        let exp = b[j..].iter().take_while(|c| c.is_ascii_digit()).count();
        if exp > 0 {
            end = j + exp;
        }
    }
    t[..end].parse().unwrap_or(f64::NAN)
}

// ---------------------------------------------------------------------------
// V8ValueStringBuilder
// ---------------------------------------------------------------------------

const MAX_ARRAY_ITEMS: u32 = 10_000;
const MAX_STACK_DEPTH: usize = 32;
const IGNORE_NULL: u8 = 1;
const IGNORE_UNDEFINED: u8 = 2;

/// One argument's message text, with the side effects Chrome has: throws
/// are swallowed (the text is then empty); termination is passed on.
fn builder_text<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    fns: &Fns<'s>,
    v: v8::Local<'s, v8::Value>,
) -> Result<String, HostError> {
    v8::tc_scope!(let tc, scope);
    let mut b = Builder {
        out: String::new(),
        limit: MAX_ARRAY_ITEMS,
        visited: Vec::new(),
    };
    let ok = b.append(tc, fns, v, 0);
    if tc.has_terminated() {
        return Err(HostError::Thrown);
    }
    let text = if ok && !tc.has_caught() {
        b.out
    } else {
        String::new()
    };
    tc.reset();
    Ok(text)
}

struct Builder {
    out: String,
    limit: u32,
    visited: Vec<v8::Global<v8::Array>>,
}

/// Call one of V8's own functions with `recv` as `this`.
fn call_builtin<'s>(
    scope: &mut v8::PinnedRef<'s, v8::TryCatch<'_, '_, v8::HandleScope<'_>>>,
    f: v8::Local<'s, v8::Function>,
    recv: v8::Local<'s, v8::Value>,
) -> Option<v8::Local<'s, v8::Value>> {
    f.call(scope, recv, &[])
}

impl Builder {
    fn append<'s>(
        &mut self,
        scope: &mut v8::PinnedRef<'s, v8::TryCatch<'_, '_, v8::HandleScope<'_>>>,
        fns: &Fns<'s>,
        v: v8::Local<'s, v8::Value>,
        ignore: u8,
    ) -> bool {
        if ignore & IGNORE_NULL != 0 && v.is_null() {
            return true;
        }
        if ignore & IGNORE_UNDEFINED != 0 && v.is_undefined() {
            return true;
        }
        let unwrap = if v.is_big_int_object() {
            Some(fns.bigint_value_of)
        } else if v.is_boolean_object() {
            Some(fns.boolean_value_of)
        } else if v.is_number_object() {
            Some(fns.number_value_of)
        } else if v.is_string_object() {
            Some(fns.string_value_of)
        } else if v.is_symbol_object() {
            Some(fns.symbol_value_of)
        } else {
            None
        };
        let v = match unwrap {
            Some(f) => match call_builtin(scope, f, v) {
                Some(p) => p,
                None => return false,
            },
            None => v,
        };
        if v.is_string() {
            return self.append_str(scope, v);
        }
        if v.is_big_int() {
            let Some(s) = v.to_string(scope) else {
                return false;
            };
            let r = self.append_str(scope, s.into());
            if scope.has_caught() {
                return false;
            }
            self.out.push('n');
            return r;
        }
        if let Ok(sym) = v8::Local::<v8::Symbol>::try_from(v) {
            self.out.push_str("Symbol(");
            let d = sym.description(scope);
            let r = self.append(scope, fns, d, IGNORE_UNDEFINED);
            self.out.push(')');
            return r;
        }
        if let Ok(arr) = v8::Local::<v8::Array>::try_from(v) {
            return self.append_array(scope, fns, arr);
        }
        if v.is_proxy() {
            self.out.push_str("[object Proxy]");
            return true;
        }
        if v.is_object()
            && !v.is_date()
            && !v.is_function()
            && !v.is_native_error()
            && !v.is_reg_exp()
        {
            if let Some(s) = call_builtin(scope, fns.object_to_string, v) {
                return self.append_str(scope, s);
            }
            if scope.has_terminated() {
                return false;
            }
        }
        match v.to_string(scope) {
            Some(s) => self.append_str(scope, s.into()),
            None => false,
        }
    }

    fn append_array<'s>(
        &mut self,
        scope: &mut v8::PinnedRef<'s, v8::TryCatch<'_, '_, v8::HandleScope<'_>>>,
        fns: &Fns<'s>,
        arr: v8::Local<'s, v8::Array>,
    ) -> bool {
        if self.visited.iter().any(|g| v8::Local::new(scope, g) == arr) {
            return true;
        }
        let len = arr.length();
        if len > self.limit || self.visited.len() > MAX_STACK_DEPTH {
            return false;
        }
        self.limit -= len;
        self.visited.push(v8::Global::new(scope, arr));
        let mut result = true;
        for i in 0..len {
            if i > 0 {
                self.out.push(',');
            }
            let Some(item) = arr.get_index(scope, i) else {
                if scope.has_terminated() {
                    result = false;
                    break;
                }
                continue;
            };
            if !self.append(scope, fns, item, IGNORE_NULL | IGNORE_UNDEFINED)
                || scope.has_terminated()
            {
                result = false;
                break;
            }
        }
        self.visited.pop();
        result
    }

    fn append_str(
        &mut self,
        scope: &mut v8::PinnedRef<'_, v8::TryCatch<'_, '_, v8::HandleScope<'_>>>,
        s: v8::Local<'_, v8::Value>,
    ) -> bool {
        if scope.has_caught() {
            return false;
        }
        self.out.push_str(&s.to_rust_string_lossy(scope));
        true
    }
}

// ---------------------------------------------------------------------------
// Our message text: data properties only
// ---------------------------------------------------------------------------

const MAX_JSON_DEPTH: usize = 64;

/// An array, or an ordinary object that is none of V8's special kinds and no
/// host wrapper. Reading its own data properties runs no page code.
fn is_plain_data(v: v8::Local<'_, v8::Value>) -> bool {
    if !v.is_object() || v.is_proxy() || v.is_function() {
        return false;
    }
    if v.is_array() {
        return true;
    }
    let Ok(o) = v8::Local::<v8::Object>::try_from(v) else {
        return false;
    };
    o.internal_field_count() == 0
        && !(v.is_date()
            || v.is_reg_exp()
            || v.is_native_error()
            || v.is_boolean_object()
            || v.is_number_object()
            || v.is_string_object()
            || v.is_symbol_object()
            || v.is_big_int_object()
            || v.is_arguments_object()
            || v.is_map()
            || v.is_set()
            || v.is_weak_map()
            || v.is_weak_set()
            || v.is_promise()
            || v.is_array_buffer()
            || v.is_array_buffer_view()
            || v.is_shared_array_buffer()
            || v.is_generator_object()
            || v.is_module_namespace_object()
            || v.is_map_iterator()
            || v.is_set_iterator())
}

/// JSON of `v`'s own enumerable data properties (accessors are left out,
/// `toJSON` is not called, other objects are `{}`); `None` for cycles,
/// deep nesting or BigInts.
fn data_json<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<'s, v8::Value>) -> Option<String> {
    v8::tc_scope!(let tc, scope);
    let mut out = String::new();
    let mut stack = Vec::new();
    let ok = json_value(tc, v, &mut stack, &mut out).is_some() && !tc.has_caught();
    tc.reset();
    ok.then_some(out)
}

/// The value of an own data property, or `None` (missing or an accessor).
fn own_data<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    obj: v8::Local<'s, v8::Object>,
    key: v8::Local<'s, v8::Name>,
) -> Option<v8::Local<'s, v8::Value>> {
    let d = obj.get_own_property_descriptor(scope, key)?;
    let d = v8::Local::<v8::Object>::try_from(d).ok()?;
    let k = v8_str(scope, "value");
    // A fresh ordinary object: `value` is an own data property or absent.
    if d.has_own_property(scope, k.into()) != Some(true) {
        return None;
    }
    d.get(scope, k.into())
}

fn json_value<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    v: v8::Local<'s, v8::Value>,
    stack: &mut Vec<v8::Global<v8::Object>>,
    out: &mut String,
) -> Option<()> {
    match value_kind(v) {
        ValueKind::Undefined | ValueKind::Function | ValueKind::Symbol | ValueKind::Null => {
            out.push_str("null")
        }
        ValueKind::Bool => out.push_str(if v.is_true() { "true" } else { "false" }),
        ValueKind::Number => {
            if v.number_value(scope).is_some_and(f64::is_finite) {
                out.push_str(&v.to_rust_string_lossy(scope));
            } else {
                out.push_str("null");
            }
        }
        ValueKind::BigInt => return None,
        ValueKind::String => push_json_string(out, &v.to_rust_string_lossy(scope)),
        ValueKind::Object => {
            if !is_plain_data(v) {
                out.push_str("{}");
                return Some(());
            }
            let obj = v8::Local::<v8::Object>::try_from(v).ok()?;
            if stack.len() >= MAX_JSON_DEPTH
                || stack.iter().any(|g| v8::Local::new(scope, g) == obj)
            {
                return None;
            }
            stack.push(v8::Global::new(scope, obj));
            let r = json_object(scope, obj, v, stack, out);
            stack.pop();
            r?;
        }
    }
    Some(())
}

fn json_object<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    obj: v8::Local<'s, v8::Object>,
    v: v8::Local<'s, v8::Value>,
    stack: &mut Vec<v8::Global<v8::Object>>,
    out: &mut String,
) -> Option<()> {
    if let Ok(arr) = v8::Local::<v8::Array>::try_from(v) {
        out.push('[');
        for i in 0..arr.length() {
            if i > 0 {
                out.push(',');
            }
            let k: v8::Local<v8::Name> = v8_str(scope, &i.to_string()).into();
            match own_data(scope, obj, k) {
                Some(item) => json_value(scope, item, stack, out)?,
                None => out.push_str("null"),
            }
        }
        out.push(']');
        return Some(());
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
            let Ok(name) = v8::Local::<v8::Name>::try_from(k) else {
                continue;
            };
            let Some(val) = own_data(scope, obj, name) else {
                continue;
            };
            if val.is_undefined() || val.is_function() || val.is_symbol() {
                continue;
            }
            if !first {
                out.push(',');
            }
            first = false;
            push_json_string(out, &k.to_rust_string_lossy(scope));
            out.push(':');
            json_value(scope, val, stack, out)?;
        }
    }
    out.push('}');
    Some(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_like_parse_int_and_parse_float() {
        assert_eq!(parse_int("  12px"), 12.0);
        assert_eq!(parse_int("-7.9"), -7.0);
        assert!(parse_int("x1").is_nan());
        assert_eq!(parse_float("1.5e3abc"), 1500.0);
        assert_eq!(parse_float(".5"), 0.5);
        assert_eq!(parse_float("-Infinity"), f64::NEG_INFINITY);
        assert_eq!(parse_float("3e"), 3.0);
        assert!(parse_float(".").is_nan());
    }
}

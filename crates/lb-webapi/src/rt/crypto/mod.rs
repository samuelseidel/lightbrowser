//! Web Crypto as in Chrome 153: `crypto.getRandomValues`, `randomUUID`
//! and `SubtleCrypto` (ADR 0025).
//!
//! The structure follows Blink and Chromium's `components/webcrypto`:
//!
//! - Bindings checks (argument counts, `CryptoKey` and `BufferSource`
//!   arguments, key formats and usages) and algorithm normalization
//!   ([`normalize`]): the same dictionaries, member order and error texts
//!   as Blink's `normalize_algorithm.cc`. Their errors carry the
//!   `Failed to execute 'x' on 'SubtleCrypto': ` prefix.
//! - The operations ([`ops`]) on BoringSSL, the library Chrome uses, with
//!   `components/webcrypto`'s checks and status texts (no prefix).
//!
//! Randomness comes from BoringSSL's `RAND_bytes` (the OS CSPRNG), never
//! from the persona's seeds. Promises settle when the method returns, as in
//! Chrome, except PBKDF2 derivations, which Chrome runs on a worker thread
//! and settles in a later task (before timers).
//! `SubtleCrypto` and `randomUUID` are `[SecureContext]`: insecure realms
//! do not have them (the generated tables' secure-only members).

mod jwk;
mod normalize;
mod ops;

use std::rc::Rc;
use std::sync::Arc;

use lb_js::{Arg, CallCtx, Handle, HostError, HostRef, v8};

pub(crate) use ops::CryptoKey;

use super::{K_OBJ, Obj, Runtime, TaskSource, Window, getter, method};
use crate::registry::Registry;
use normalize::{Alg, NErr, Op, Params};

/// Crypto host objects.
pub(crate) enum CryptoObj {
    Crypto,
    Subtle,
    Key(Arc<CryptoKey>),
}

impl CryptoObj {
    pub(crate) fn iface_name(&self) -> &'static str {
        match self {
            CryptoObj::Crypto => "Crypto",
            CryptoObj::Subtle => "SubtleCrypto",
            CryptoObj::Key(_) => "CryptoKey",
        }
    }
}

/// Key usages, in Chromium's order (`key_ops`, `CryptoKey.usages`).
pub(crate) const USAGES: [&str; 8] = [
    "encrypt",
    "decrypt",
    "sign",
    "verify",
    "deriveKey",
    "deriveBits",
    "wrapKey",
    "unwrapKey",
];
pub(crate) const ENCRYPT: u8 = 1;
pub(crate) const DECRYPT: u8 = 2;
pub(crate) const SIGN: u8 = 4;
pub(crate) const VERIFY: u8 = 8;
pub(crate) const DERIVE_KEY: u8 = 16;
pub(crate) const DERIVE_BITS: u8 = 32;
pub(crate) const WRAP: u8 = 64;
pub(crate) const UNWRAP: u8 = 128;

/// Usage names of a mask, in order.
pub(crate) fn usage_names(mask: u8) -> Vec<&'static str> {
    USAGES
        .iter()
        .enumerate()
        .filter(|(i, _)| mask & (1 << i) != 0)
        .map(|(_, n)| *n)
        .collect()
}

/// A failed operation: a `DOMException` (name, message), a `TypeError`
/// or a caught JS exception.
pub(crate) enum Fail {
    Dom(&'static str, String),
    Type(String),
    Exc(Handle),
}

impl Fail {
    pub(crate) fn op(msg: impl Into<String>) -> Fail {
        Fail::Dom("OperationError", msg.into())
    }
    pub(crate) fn data(msg: impl Into<String>) -> Fail {
        Fail::Dom("DataError", msg.into())
    }
    pub(crate) fn access(msg: impl Into<String>) -> Fail {
        Fail::Dom("InvalidAccessError", msg.into())
    }
    pub(crate) fn syntax(msg: impl Into<String>) -> Fail {
        Fail::Dom("SyntaxError", msg.into())
    }
    pub(crate) fn unsupported(msg: impl Into<String>) -> Fail {
        Fail::Dom("NotSupportedError", msg.into())
    }
}

/// A successful operation's value.
enum Done {
    Bytes(Vec<u8>),
    Bool(bool),
    Key(Arc<CryptoKey>),
    Pair(Arc<CryptoKey>, Arc<CryptoKey>),
    Json(String),
    Undefined,
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    for global in ["Window", "WorkerGlobalScope"] {
        getter(reg, rt, global, "crypto", |cx, w| {
            let id = w.cached_obj((0, 0, "crypto", String::new()), || {
                Obj::Crypto(Box::new(CryptoObj::Crypto))
            });
            cx.ret(w.obj_arg(id))
        });
    }
    getter(reg, rt, "Crypto", "subtle", |cx, w| {
        this_is(cx, w, |o| matches!(o, CryptoObj::Crypto))?;
        let id = w.cached_obj((0, 0, "crypto.subtle", String::new()), || {
            Obj::Crypto(Box::new(CryptoObj::Subtle))
        });
        cx.ret(w.obj_arg(id))
    });
    method(reg, rt, "Crypto", "getRandomValues", get_random_values);
    method(reg, rt, "Crypto", "randomUUID", |cx, w| {
        this_is(cx, w, |o| matches!(o, CryptoObj::Crypto))?;
        let mut b = [0u8; 16];
        ops::random(&mut b);
        b[6] = (b[6] & 0x0f) | 0x40;
        b[8] = (b[8] & 0x3f) | 0x80;
        let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
        cx.ret(format!(
            "{}-{}-{}-{}-{}",
            &h[0..8],
            &h[8..12],
            &h[12..16],
            &h[16..20],
            &h[20..32]
        ))
    });
    for m in [
        "encrypt",
        "decrypt",
        "sign",
        "verify",
        "digest",
        "generateKey",
        "deriveKey",
        "deriveBits",
        "importKey",
        "exportKey",
        "wrapKey",
        "unwrapKey",
    ] {
        method(reg, rt, "SubtleCrypto", m, move |cx, w| subtle(cx, w, m));
    }
    getter(reg, rt, "CryptoKey", "type", |cx, w| {
        let k = this_key(cx, w)?;
        cx.ret(k.ty.name())
    });
    getter(reg, rt, "CryptoKey", "extractable", |cx, w| {
        let k = this_key(cx, w)?;
        cx.ret(k.extractable)
    });
    getter(reg, rt, "CryptoKey", "algorithm", |cx, w| {
        let k = this_key(cx, w)?;
        let h = algorithm_object(cx, &k)?;
        cx.ret(Arg::Handle(h))?;
        cx.release(h);
        Ok(())
    });
    getter(reg, rt, "CryptoKey", "usages", |cx, w| {
        let k = this_key(cx, w)?;
        let list = usage_names(k.usages)
            .into_iter()
            .map(|u| Arg::String(u.to_owned()))
            .collect();
        cx.ret(Arg::Array(list))
    });
    // `QuotaExceededError` (getRandomValues): a `DOMException` subclass.
    reg.constructor(
        "QuotaExceededError",
        crate::registry::host_fn(|cx| {
            let message = if cx.is_empty() || cx.arg_kind(0) == lb_js::ValueKind::Undefined {
                String::new()
            } else {
                cx.arg_string(0)?
            };
            cx.set_this_private("lb-webapi:DOMException#message", message)?;
            cx.set_this_private(
                "lb-webapi:DOMException#name",
                "QuotaExceededError".to_owned(),
            )
        }),
    );
    for name in ["quota", "requested"] {
        reg.getter(
            "QuotaExceededError",
            name,
            crate::registry::host_fn(|cx| cx.ret(Arg::Null)),
        );
    }
}

/// `this` must be a crypto object `f` accepts.
fn this_is(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    f: impl FnOnce(&CryptoObj) -> bool,
) -> Result<(), HostError> {
    let ok = cx
        .this_host()
        .filter(|h| h.kind == K_OBJ)
        .and_then(|h| {
            w.with_obj(h.id, |o| match o {
                Obj::Crypto(c) => f(c),
                _ => false,
            })
        })
        .unwrap_or(false);
    if ok { Ok(()) } else { Err(super::illegal()) }
}

fn key_of(w: &Window, h: HostRef) -> Option<Arc<CryptoKey>> {
    if h.kind != K_OBJ {
        return None;
    }
    w.with_obj(h.id, |o| match o {
        Obj::Crypto(c) => match &**c {
            CryptoObj::Key(k) => Some(k.clone()),
            _ => None,
        },
        _ => None,
    })
    .flatten()
}

fn this_key(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<Arc<CryptoKey>, HostError> {
    cx.this_host()
        .and_then(|h| key_of(w, h))
        .ok_or_else(super::illegal)
}

/// A new `CryptoKey` object of `w`.
pub(crate) fn key_arg(w: &Window, k: Arc<CryptoKey>) -> Arg {
    let id = w.new_obj(Obj::Crypto(Box::new(CryptoObj::Key(k))));
    w.obj_arg(id)
}

/// The key behind a host object (structured clone).
pub(crate) fn clone_key(w: &Window, h: HostRef) -> Option<Arc<CryptoKey>> {
    key_of(w, h)
}

/// `key.algorithm`: a new object on each read, as in Chrome.
fn algorithm_object(cx: &mut CallCtx<'_, '_, '_>, k: &CryptoKey) -> Result<Handle, HostError> {
    let a = &k.alg;
    let name = a.id.name();
    let mut json = format!("{{\"name\":\"{name}\"");
    if let Some(h) = a.hash {
        json.push_str(&format!(",\"hash\":{{\"name\":\"{}\"}}", h.name()));
    }
    match a.id {
        Alg::AesGcm | Alg::AesCbc | Alg::AesCtr | Alg::AesKw | Alg::Hmac => {
            json.push_str(&format!(",\"length\":{}", a.length));
        }
        Alg::Ecdsa | Alg::Ecdh => {
            if let Some(c) = a.curve {
                json.push_str(&format!(",\"namedCurve\":\"{}\"", c.name()));
            }
        }
        Alg::RsaPkcs1 | Alg::RsaPss | Alg::RsaOaep => {
            json.push_str(&format!(",\"modulusLength\":{}", a.modulus));
        }
        _ => {}
    }
    json.push('}');
    let h = cx.make_handle(Arg::Json(json))?;
    if matches!(a.id, Alg::RsaPkcs1 | Alg::RsaPss | Alg::RsaOaep) {
        cx.set_property(h, "publicExponent", Arg::Uint8Array(a.exponent.clone()))?;
    }
    Ok(h)
}

// ---------------------------------------------------------------------------
// crypto.getRandomValues
// ---------------------------------------------------------------------------

fn get_random_values(cx: &mut CallCtx<'_, '_, '_>, w: &Rc<Window>) -> Result<(), HostError> {
    this_is(cx, w, |o| matches!(o, CryptoObj::Crypto))?;
    const PFX: &str = "Failed to execute 'getRandomValues' on 'Crypto': ";
    if cx.is_empty() {
        return Err(HostError::type_error(format!(
            "{PFX}1 argument required, but only 0 present."
        )));
    }
    let v = cx.v8_args().get(0);
    let Ok(view) = v8::Local::<v8::ArrayBufferView>::try_from(v) else {
        return Err(HostError::type_error(format!(
            "{PFX}parameter 1 is not of type 'ArrayBufferView'."
        )));
    };
    let kind = if v.is_data_view() {
        Some("DataView")
    } else if v.is_float16_array() {
        Some("Float16")
    } else if v.is_float32_array() {
        Some("Float32")
    } else if v.is_float64_array() {
        Some("Float64")
    } else {
        None
    };
    if let Some(kind) = kind {
        return Err(w.rt.err(
            "TypeMismatchError",
            format!(
                "{PFX}The provided ArrayBufferView is of type '{kind}', which is not an integer array type."
            ),
        ));
    }
    let len = view.byte_length();
    if len > 65536 {
        let msg = format!(
            "{PFX}The ArrayBufferView's byte length ({len}) exceeds the number of bytes of entropy available via this API (65536)."
        );
        return Err(HostError::Throw(Arg::Construct(
            w.rt.iface("QuotaExceededError"),
            vec![Arg::String(msg)],
        )));
    }
    let mut bytes = vec![0u8; len];
    ops::random(&mut bytes);
    cx.write_arg_bytes(0, 0, &bytes);
    cx.ret_v8(v);
    Ok(())
}

// ---------------------------------------------------------------------------
// SubtleCrypto
// ---------------------------------------------------------------------------

fn subtle(cx: &mut CallCtx<'_, '_, '_>, w: &Rc<Window>, m: &'static str) -> Result<(), HostError> {
    let resolver = cx.ret_new_promise()?;
    let pfx = format!("Failed to execute '{m}' on 'SubtleCrypto': ");
    let mut slow = false;
    let outcome = if this_is(cx, w, |o| matches!(o, CryptoObj::Subtle)).is_err() {
        Err(Fail::Type(format!("{pfx}Illegal invocation")))
    } else {
        run(cx, w, m, &pfx, &mut slow)
    };
    let settled: Result<Arg, HostError> = match outcome {
        Ok(d) => done_arg(cx, w, d),
        Err(Fail::Dom(name, msg)) => Err(w.rt.err(name, msg)),
        Err(Fail::Type(msg)) => Err(HostError::TypeError(msg)),
        Err(Fail::Exc(h)) => Err(HostError::Throw(Arg::Handle(h))),
    };
    if slow {
        // Chrome runs PBKDF2 on a worker thread and settles in a task.
        w.queue_task(
            TaskSource::DomManipulation,
            Box::new(move |js, _w| {
                let held = match &settled {
                    Ok(Arg::Handle(h)) => Some(*h),
                    _ => None,
                };
                match settled {
                    Ok(v) => js.resolve(resolver, v),
                    Err(e) => js.reject(resolver, e),
                }
                if let Some(h) = held {
                    js.release(h);
                }
            }),
        );
        return Ok(());
    }
    let held = match &settled {
        Ok(Arg::Handle(h)) | Err(HostError::Throw(Arg::Handle(h))) => Some(*h),
        _ => None,
    };
    match settled {
        Ok(v) => cx.resolve_promise(resolver, v)?,
        Err(e) => cx.reject_promise(resolver, e)?,
    }
    if let Some(h) = held {
        cx.release(h);
    }
    Ok(())
}

fn done_arg(cx: &mut CallCtx<'_, '_, '_>, w: &Window, d: Done) -> Result<Arg, HostError> {
    Ok(match d {
        Done::Bytes(b) => Arg::ArrayBuffer(b),
        Done::Bool(b) => Arg::Bool(b),
        Done::Key(k) => key_arg(w, k),
        Done::Pair(private, public) => {
            let h = cx.make_handle(Arg::Json("{}".into()))?;
            cx.set_property(h, "privateKey", key_arg(w, private))?;
            cx.set_property(h, "publicKey", key_arg(w, public))?;
            Arg::Handle(h)
        }
        Done::Json(s) => Arg::Json(s),
        Done::Undefined => Arg::Undefined,
    })
}

/// Run a JS-reading step in a `TryCatch`: an exception becomes
/// [`Fail::Exc`], normalization errors get the method's prefix.
fn read<T>(
    cx: &mut CallCtx<'_, '_, '_>,
    pfx: &str,
    f: impl for<'s, 'i> FnOnce(&mut v8::PinScope<'s, 'i>) -> Result<T, NErr>,
) -> Result<T, Fail> {
    let r = {
        let scope = cx.scope();
        v8::tc_scope!(let tc, scope);
        match f(tc) {
            Ok(v) => Ok(v),
            Err(NErr::Thrown) => {
                let exc = tc.exception().map(|e| v8::Global::new(tc, e));
                Err(Err(exc))
            }
            Err(e) => Err(Ok(e)),
        }
    };
    match r {
        Ok(v) => Ok(v),
        Err(Ok(NErr::Type(m))) => Err(Fail::Type(format!("{pfx}{m}"))),
        Err(Ok(NErr::NotSupported(m))) => Err(Fail::Dom("NotSupportedError", format!("{pfx}{m}"))),
        Err(Ok(NErr::Thrown)) => unreachable!(),
        Err(Err(exc)) => {
            let Some(g) = exc else {
                return Err(Fail::Type(format!("{pfx}unknown error")));
            };
            let local = v8::Local::new(cx.scope(), &g);
            Err(Fail::Exc(cx.handle_of(local)))
        }
    }
}

/// Bytes of a `BufferSource` argument.
fn buffer_arg(cx: &mut CallCtx<'_, '_, '_>, i: usize, pfx: &str) -> Result<Vec<u8>, Fail> {
    cx.arg_bytes(i).ok_or_else(|| {
        Fail::Type(format!(
            "{pfx}The provided value is not of type '(ArrayBuffer or ArrayBufferView)'."
        ))
    })
}

fn key_arg_at(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    i: usize,
    pfx: &str,
) -> Result<Arc<CryptoKey>, Fail> {
    cx.arg_host(i).and_then(|h| key_of(w, h)).ok_or_else(|| {
        Fail::Type(format!(
            "{pfx}parameter {} is not of type 'CryptoKey'.",
            i + 1
        ))
    })
}

fn string_arg(cx: &mut CallCtx<'_, '_, '_>, i: usize, pfx: &str) -> Result<String, Fail> {
    let g = arg_global(cx, i);
    read(cx, pfx, move |s| {
        let v = v8::Local::new(s, &g);
        normalize::to_string(s, v)
    })
}

/// Argument `i` held across a `TryCatch` (`undefined` when missing).
fn arg_global(cx: &mut CallCtx<'_, '_, '_>, i: usize) -> v8::Global<v8::Value> {
    let v = cx.v8_args().get(i as i32);
    v8::Global::new(cx.scope(), v)
}

fn bool_arg(cx: &mut CallCtx<'_, '_, '_>, i: usize) -> bool {
    let v = cx.v8_args().get(i as i32);
    v.boolean_value(cx.scope())
}

/// `sequence<DOMString>` key usages, then Chrome's parse into a mask.
fn usages_arg(cx: &mut CallCtx<'_, '_, '_>, i: usize, pfx: &str) -> Result<u8, Fail> {
    let g = arg_global(cx, i);
    let list = read(cx, pfx, move |s| {
        let v = v8::Local::new(s, &g);
        normalize::string_sequence(s, v)
    })?;
    let mut mask = 0u8;
    for u in list {
        match USAGES.iter().position(|n| *n == u) {
            Some(i) => mask |= 1 << i,
            None => return Err(Fail::Type(format!("{pfx}Invalid keyUsages argument"))),
        }
    }
    Ok(mask)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Format {
    Raw,
    Pkcs8,
    Spki,
    Jwk,
}

fn format_arg(cx: &mut CallCtx<'_, '_, '_>, i: usize, pfx: &str) -> Result<Format, Fail> {
    let s = string_arg(cx, i, pfx)?;
    Ok(match s.as_str() {
        "raw" => Format::Raw,
        "pkcs8" => Format::Pkcs8,
        "spki" => Format::Spki,
        "jwk" => Format::Jwk,
        _ => return Err(Fail::Type(format!("{pfx}Invalid keyFormat argument: {s}"))),
    })
}

/// `CryptoKey::CanBeUsedForAlgorithm` in Blink.
fn check_key(key: &CryptoKey, alg: Alg, usage: u8) -> Result<(), Fail> {
    if key.alg.id != alg {
        return Err(Fail::access(
            "key.algorithm does not match that of operation",
        ));
    }
    if key.usages & usage == 0 {
        return Err(Fail::access("key.usages does not permit this operation"));
    }
    Ok(())
}

fn need(cx: &CallCtx<'_, '_, '_>, n: usize, pfx: &str) -> Result<(), Fail> {
    if cx.len() < n {
        let s = if n == 1 { "argument" } else { "arguments" };
        return Err(Fail::Type(format!(
            "{pfx}{n} {s} required, but only {} present.",
            cx.len()
        )));
    }
    Ok(())
}

/// One `SubtleCrypto` method. `slow`: Chrome settles it in a later task
/// (set once the arguments are normalized).
fn run(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Rc<Window>,
    m: &'static str,
    pfx: &str,
    slow: &mut bool,
) -> Result<Done, Fail> {
    match m {
        "digest" => {
            need(cx, 2, pfx)?;
            let id = algorithm_id(cx, 0, pfx)?;
            let data = buffer_arg(cx, 1, pfx)?;
            let p = normalize_held(cx, w, id, Op::Digest, pfx)?;
            Ok(Done::Bytes(ops::digest(p.alg, &data)))
        }
        "encrypt" | "decrypt" => {
            need(cx, 3, pfx)?;
            let id = algorithm_id(cx, 0, pfx)?;
            let key = key_arg_at(cx, w, 1, pfx)?;
            let data = buffer_arg(cx, 2, pfx)?;
            let (op, usage) = if m == "encrypt" {
                (Op::Encrypt, ENCRYPT)
            } else {
                (Op::Decrypt, DECRYPT)
            };
            let p = normalize_held(cx, w, id, op, pfx)?;
            check_key(&key, p.alg, usage)?;
            let out = if m == "encrypt" {
                ops::encrypt(&p, &key, &data)?
            } else {
                ops::decrypt(&p, &key, &data)?
            };
            Ok(Done::Bytes(out))
        }
        "sign" => {
            need(cx, 3, pfx)?;
            let id = algorithm_id(cx, 0, pfx)?;
            let key = key_arg_at(cx, w, 1, pfx)?;
            let data = buffer_arg(cx, 2, pfx)?;
            let p = normalize_held(cx, w, id, Op::Sign, pfx)?;
            check_key(&key, p.alg, SIGN)?;
            Ok(Done::Bytes(ops::sign(&p, &key, &data)?))
        }
        "verify" => {
            need(cx, 4, pfx)?;
            let id = algorithm_id(cx, 0, pfx)?;
            let key = key_arg_at(cx, w, 1, pfx)?;
            let sig = buffer_arg(cx, 2, pfx)?;
            let data = buffer_arg(cx, 3, pfx)?;
            let p = normalize_held(cx, w, id, Op::Verify, pfx)?;
            check_key(&key, p.alg, VERIFY)?;
            Ok(Done::Bool(ops::verify(&p, &key, &sig, &data)?))
        }
        "generateKey" => {
            need(cx, 3, pfx)?;
            let id = algorithm_id(cx, 0, pfx)?;
            let extractable = bool_arg(cx, 1);
            let usages = usages_arg(cx, 2, pfx)?;
            let p = normalize_held(cx, w, id, Op::GenerateKey, pfx)?;
            Ok(match ops::generate(&p, extractable, usages)? {
                ops::Generated::Secret(k) => Done::Key(Arc::new(k)),
                ops::Generated::Pair(private, public) => {
                    Done::Pair(Arc::new(private), Arc::new(public))
                }
            })
        }
        "importKey" => {
            need(cx, 5, pfx)?;
            let format = format_arg(cx, 0, pfx)?;
            let data = key_data_arg(cx, 1, format, pfx)?;
            let id = algorithm_id(cx, 2, pfx)?;
            let extractable = bool_arg(cx, 3);
            let usages = usages_arg(cx, 4, pfx)?;
            let p = normalize_held(cx, w, id, Op::ImportKey, pfx)?;
            let k = ops::import(format, &data, &p, extractable, usages)?;
            Ok(Done::Key(Arc::new(k)))
        }
        "exportKey" => {
            need(cx, 2, pfx)?;
            let format = format_arg(cx, 0, pfx)?;
            let key = key_arg_at(cx, w, 1, pfx)?;
            if !key.extractable {
                return Err(Fail::access(format!("{pfx}key is not extractable")));
            }
            Ok(match ops::export(format, &key)? {
                ops::Exported::Bytes(b) => Done::Bytes(b),
                ops::Exported::Jwk(s) => Done::Json(s),
            })
        }
        "deriveBits" => {
            need(cx, 2, pfx)?;
            let id = algorithm_id(cx, 0, pfx)?;
            let key = key_arg_at(cx, w, 1, pfx)?;
            let length = optional_length(cx, 2);
            let p = normalize_held(cx, w, id, Op::DeriveBits, pfx)?;
            check_key(&key, p.alg, DERIVE_BITS)?;
            *slow = p.alg == Alg::Pbkdf2;
            Ok(Done::Bytes(ops::derive_bits(&p, &key, length)?))
        }
        "deriveKey" => {
            need(cx, 5, pfx)?;
            let id = algorithm_id(cx, 0, pfx)?;
            let key = key_arg_at(cx, w, 1, pfx)?;
            let derived = algorithm_id(cx, 2, pfx)?;
            let extractable = bool_arg(cx, 3);
            let usages = usages_arg(cx, 4, pfx)?;
            let p = normalize_held(cx, w, id, Op::DeriveBits, pfx)?;
            let import = normalize_held(cx, w, derived.clone(), Op::ImportKey, pfx)?;
            let length = normalize_held(cx, w, derived, Op::GetKeyLength, pfx)?;
            *slow = p.alg == Alg::Pbkdf2;
            check_key(&key, p.alg, DERIVE_KEY)?;
            let bits = ops::key_length(&length)?;
            let raw = ops::derive_bits(&p, &key, bits)?;
            let k = ops::import(
                Format::Raw,
                &KeyData::Bytes(raw),
                &import,
                extractable,
                usages,
            )?;
            Ok(Done::Key(Arc::new(k)))
        }
        "wrapKey" => {
            need(cx, 4, pfx)?;
            let format = format_arg(cx, 0, pfx)?;
            let key = key_arg_at(cx, w, 1, pfx)?;
            let wrapping = key_arg_at(cx, w, 2, pfx)?;
            let id = algorithm_id(cx, 3, pfx)?;
            let p = normalize_held(cx, w, id, Op::WrapKey, pfx)?;
            check_key(&wrapping, p.alg, WRAP)?;
            if !key.extractable {
                return Err(Fail::access("key is not extractable"));
            }
            let bytes = match ops::export(format, &key)? {
                ops::Exported::Bytes(b) => b,
                ops::Exported::Jwk(s) => s.into_bytes(),
            };
            Ok(Done::Bytes(ops::wrap(&p, &wrapping, &bytes)?))
        }
        "unwrapKey" => {
            need(cx, 7, pfx)?;
            let format = format_arg(cx, 0, pfx)?;
            let wrapped = buffer_arg(cx, 1, pfx)?;
            let unwrapping = key_arg_at(cx, w, 2, pfx)?;
            let id = algorithm_id(cx, 3, pfx)?;
            let kid = algorithm_id(cx, 4, pfx)?;
            let extractable = bool_arg(cx, 5);
            let usages = usages_arg(cx, 6, pfx)?;
            let p = normalize_held(cx, w, id, Op::UnwrapKey, pfx)?;
            let kp = normalize_held(cx, w, kid, Op::ImportKey, pfx)?;
            check_key(&unwrapping, p.alg, UNWRAP)?;
            let bytes = ops::unwrap(&p, &unwrapping, &wrapped)?;
            let data = if format == Format::Jwk {
                KeyData::Jwk(Box::new(jwk::parse_json(&bytes)?))
            } else {
                KeyData::Bytes(bytes)
            };
            let k = ops::import(format, &data, &kp, extractable, usages)?;
            Ok(Done::Key(Arc::new(k)))
        }
        _ => Ok(Done::Undefined),
    }
}

/// An `AlgorithmIdentifier` argument as the bindings convert it: an
/// object stays as is (held), anything else becomes a string.
#[derive(Clone)]
enum AlgId {
    Name(String),
    Object(Rc<v8::Global<v8::Value>>),
}

fn algorithm_id(cx: &mut CallCtx<'_, '_, '_>, i: usize, pfx: &str) -> Result<AlgId, Fail> {
    let v = cx.v8_args().get(i as i32);
    if v.is_object() {
        let g = v8::Global::new(cx.scope(), v);
        return Ok(AlgId::Object(Rc::new(g)));
    }
    let s = string_arg(cx, i, pfx)?;
    Ok(AlgId::Name(s))
}

fn normalize_held(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    id: AlgId,
    op: Op,
    pfx: &str,
) -> Result<Params, Fail> {
    match id {
        AlgId::Name(name) => {
            let r = read(cx, pfx, move |s| {
                normalize::normalize_name(s, &name, None, op, "")
            })?;
            Ok(r.params)
        }
        AlgId::Object(g) => {
            let raw = read(cx, pfx, move |s| {
                let v = v8::Local::new(s, &*g);
                normalize::normalize(s, v, op, "")
            })?;
            let public = match raw.public {
                Some(pg) => {
                    let local = v8::Local::new(cx.scope(), &pg);
                    let host = lb_js::host_of_value(cx.scope(), local);
                    match host.and_then(|h| key_of(w, h)) {
                        Some(k) => Some(k),
                        None => {
                            return Err(Fail::Type(format!(
                                "{pfx}EcdhKeyDeriveParams: public: Must be a CryptoKey"
                            )));
                        }
                    }
                }
                None => None,
            };
            Ok(Params {
                public_key: public,
                ..raw.params
            })
        }
    }
}

/// `optional unsigned long? length = null`.
fn optional_length(cx: &mut CallCtx<'_, '_, '_>, i: usize) -> Option<u32> {
    if cx.len() <= i {
        return None;
    }
    let v = cx.v8_args().get(i as i32);
    if v.is_null_or_undefined() {
        return None;
    }
    let n = v.number_value(cx.scope()).unwrap_or(0.0);
    if !n.is_finite() {
        return Some(0);
    }
    Some((n.trunc().rem_euclid(4294967296.0)) as u32)
}

/// Key data of `importKey`: a `BufferSource` or (for JWK) a dictionary.
pub(crate) enum KeyData {
    Bytes(Vec<u8>),
    Jwk(Box<jwk::Jwk>),
}

fn key_data_arg(
    cx: &mut CallCtx<'_, '_, '_>,
    i: usize,
    format: Format,
    pfx: &str,
) -> Result<KeyData, Fail> {
    let bytes = cx.arg_bytes(i);
    let v = cx.v8_args().get(i as i32);
    if bytes.is_none() && !v.is_object() {
        return Err(Fail::Type(format!(
            "{pfx}The provided value is not of type '(ArrayBuffer or ArrayBufferView or object)'."
        )));
    }
    match (format, bytes) {
        (Format::Jwk, Some(_)) => Err(Fail::Type(format!(
            "{pfx}Key data must be an object for JWK import"
        ))),
        (Format::Jwk, None) => {
            let g = arg_global(cx, i);
            let j = read(cx, pfx, move |s| {
                let v = v8::Local::new(s, &g);
                jwk::read_dict(s, v)
            })?;
            Ok(KeyData::Jwk(Box::new(j)))
        }
        (_, Some(b)) => Ok(KeyData::Bytes(b)),
        (_, None) => Err(Fail::Type(format!(
            "{pfx}Key data must be a BufferSource for non-JWK formats"
        ))),
    }
}

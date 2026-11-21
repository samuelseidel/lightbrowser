//! Algorithm normalization as Blink's `normalize_algorithm.cc`: the
//! registered algorithms, which operation takes which parameter
//! dictionary, the dictionaries' members (read in Blink's order, present
//! when the object *has* the property) and the error texts.

use std::sync::Arc;

use lb_js::v8;

use super::CryptoKey;

/// A normalization error (prefixed by the caller) or a pending JS
/// exception.
pub(crate) enum NErr {
    Type(String),
    NotSupported(String),
    Thrown,
}

pub(crate) type NRes<T> = Result<T, NErr>;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum Alg {
    #[default]
    Sha1,
    Sha256,
    Sha384,
    Sha512,
    Hmac,
    AesCbc,
    AesGcm,
    AesCtr,
    AesKw,
    RsaPkcs1,
    RsaPss,
    RsaOaep,
    Ecdsa,
    Ecdh,
    Hkdf,
    Pbkdf2,
    Ed25519,
    X25519,
}

const ALGS: &[(Alg, &str)] = &[
    (Alg::AesCbc, "AES-CBC"),
    (Alg::Hmac, "HMAC"),
    (Alg::RsaPkcs1, "RSASSA-PKCS1-v1_5"),
    (Alg::Sha1, "SHA-1"),
    (Alg::Sha256, "SHA-256"),
    (Alg::Sha384, "SHA-384"),
    (Alg::Sha512, "SHA-512"),
    (Alg::AesGcm, "AES-GCM"),
    (Alg::RsaOaep, "RSA-OAEP"),
    (Alg::AesCtr, "AES-CTR"),
    (Alg::AesKw, "AES-KW"),
    (Alg::RsaPss, "RSA-PSS"),
    (Alg::Ecdsa, "ECDSA"),
    (Alg::Ecdh, "ECDH"),
    (Alg::Hkdf, "HKDF"),
    (Alg::Pbkdf2, "PBKDF2"),
    (Alg::Ed25519, "Ed25519"),
    (Alg::X25519, "X25519"),
];

impl Alg {
    pub(crate) fn name(self) -> &'static str {
        ALGS.iter().find(|(a, _)| *a == self).map_or("", |(_, n)| n)
    }
    fn from_name(s: &str) -> Option<Alg> {
        ALGS.iter()
            .find(|(_, n)| n.eq_ignore_ascii_case(s))
            .map(|(a, _)| *a)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Curve {
    P256,
    P384,
    P521,
}

impl Curve {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Curve::P256 => "P-256",
            Curve::P384 => "P-384",
            Curve::P521 => "P-521",
        }
    }
    pub(crate) fn from_name(s: &str) -> Option<Curve> {
        [Curve::P256, Curve::P384, Curve::P521]
            .into_iter()
            .find(|c| c.name() == s)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Op {
    Encrypt,
    Decrypt,
    Sign,
    Verify,
    Digest,
    GenerateKey,
    ImportKey,
    GetKeyLength,
    DeriveBits,
    WrapKey,
    UnwrapKey,
}

impl Op {
    fn name(self) -> &'static str {
        match self {
            Op::Encrypt => "encrypt",
            Op::Decrypt => "decrypt",
            Op::Sign => "sign",
            Op::Verify => "verify",
            Op::Digest => "digest",
            Op::GenerateKey => "generateKey",
            Op::ImportKey => "importKey",
            Op::GetKeyLength => "get key length",
            Op::DeriveBits => "deriveBits",
            Op::WrapKey => "wrapKey",
            Op::UnwrapKey => "unwrapKey",
        }
    }
}

/// A normalized algorithm with its parameters.
#[derive(Clone, Default)]
pub(crate) struct Params {
    pub alg: Alg,
    pub hash: Option<Alg>,
    pub iv: Option<Vec<u8>>,
    pub additional_data: Option<Vec<u8>>,
    pub tag_length: Option<u32>,
    pub counter: Option<Vec<u8>>,
    pub length: Option<u32>,
    pub curve: Option<Curve>,
    pub salt: Option<Vec<u8>>,
    pub info: Option<Vec<u8>>,
    pub iterations: u32,
    pub salt_length: u32,
    pub label: Option<Vec<u8>>,
    pub modulus_length: u32,
    pub public_exponent: Vec<u8>,
    pub public_key: Option<Arc<CryptoKey>>,
}

/// [`Params`] plus the raw `public` member of `EcdhKeyDeriveParams` (the
/// caller resolves it to a key).
pub(crate) struct Normalized {
    pub params: Params,
    pub public: Option<v8::Global<v8::Value>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Dict {
    None,
    AesCbc,
    Aead,
    AesCtr,
    AesKeyGen,
    AesDerived,
    HmacImport,
    HmacKeyGen,
    RsaHashedKeyGen,
    RsaHashedImport,
    RsaPss,
    RsaOaep,
    Ecdsa,
    EcKeyGen,
    EcKeyImport,
    EcdhDerive,
    Hkdf,
    Pbkdf2,
}

impl Dict {
    fn name(self) -> &'static str {
        match self {
            Dict::None => "",
            Dict::AesCbc => "AesCbcParams",
            Dict::Aead => "AeadParams",
            Dict::AesCtr => "AesCtrParams",
            Dict::AesKeyGen => "AesKeyGenParams",
            Dict::AesDerived => "AesDerivedKeyParams",
            Dict::HmacImport => "HmacImportParams",
            Dict::HmacKeyGen => "HmacKeyGenParams",
            Dict::RsaHashedKeyGen => "RsaHashedKeyGenParams",
            Dict::RsaHashedImport => "RsaHashedImportParams",
            Dict::RsaPss => "RsaPssParams",
            Dict::RsaOaep => "RsaOaepParams",
            Dict::Ecdsa => "EcdsaParams",
            Dict::EcKeyGen => "EcKeyGenParams",
            Dict::EcKeyImport => "EcKeyImportParams",
            Dict::EcdhDerive => "EcdhKeyDeriveParams",
            Dict::Hkdf => "HkdfParams",
            Dict::Pbkdf2 => "Pbkdf2Params",
        }
    }
}

/// The parameter dictionary `alg` takes for `op`; `None`: unsupported.
fn dict_for(alg: Alg, op: Op) -> Option<Dict> {
    use Op::*;
    Some(match (alg, op) {
        (Alg::Sha1 | Alg::Sha256 | Alg::Sha384 | Alg::Sha512, Digest) => Dict::None,
        (Alg::AesCbc, Encrypt | Decrypt | WrapKey | UnwrapKey) => Dict::AesCbc,
        (Alg::AesGcm, Encrypt | Decrypt | WrapKey | UnwrapKey) => Dict::Aead,
        (Alg::AesCtr, Encrypt | Decrypt | WrapKey | UnwrapKey) => Dict::AesCtr,
        (Alg::AesKw, WrapKey | UnwrapKey) => Dict::None,
        (Alg::AesCbc | Alg::AesGcm | Alg::AesCtr | Alg::AesKw, GenerateKey) => Dict::AesKeyGen,
        (Alg::AesCbc | Alg::AesGcm | Alg::AesCtr | Alg::AesKw, ImportKey) => Dict::None,
        (Alg::AesCbc | Alg::AesGcm | Alg::AesCtr | Alg::AesKw, GetKeyLength) => Dict::AesDerived,
        (Alg::Hmac, Sign | Verify) => Dict::None,
        (Alg::Hmac, GenerateKey) => Dict::HmacKeyGen,
        (Alg::Hmac, ImportKey | GetKeyLength) => Dict::HmacImport,
        (Alg::RsaPkcs1, Sign | Verify) => Dict::None,
        (Alg::RsaPss, Sign | Verify) => Dict::RsaPss,
        (Alg::RsaOaep, Encrypt | Decrypt | WrapKey | UnwrapKey) => Dict::RsaOaep,
        (Alg::RsaPkcs1 | Alg::RsaPss | Alg::RsaOaep, GenerateKey) => Dict::RsaHashedKeyGen,
        (Alg::RsaPkcs1 | Alg::RsaPss | Alg::RsaOaep, ImportKey) => Dict::RsaHashedImport,
        (Alg::Ecdsa, Sign | Verify) => Dict::Ecdsa,
        (Alg::Ecdsa | Alg::Ecdh, GenerateKey) => Dict::EcKeyGen,
        (Alg::Ecdsa | Alg::Ecdh, ImportKey) => Dict::EcKeyImport,
        (Alg::Ecdh, DeriveBits) => Dict::EcdhDerive,
        (Alg::Hkdf, DeriveBits) => Dict::Hkdf,
        (Alg::Pbkdf2, DeriveBits) => Dict::Pbkdf2,
        (Alg::Hkdf | Alg::Pbkdf2, ImportKey | GetKeyLength) => Dict::None,
        (Alg::Ed25519, Sign | Verify | GenerateKey | ImportKey) => Dict::None,
        (Alg::X25519, DeriveBits) => Dict::EcdhDerive,
        (Alg::X25519, GenerateKey | ImportKey) => Dict::None,
        _ => return None,
    })
}

/// ToString of a JS value.
pub(crate) fn to_string(s: &mut v8::PinScope<'_, '_>, v: v8::Local<'_, v8::Value>) -> NRes<String> {
    let st = v.to_string(s).ok_or(NErr::Thrown)?;
    Ok(st.to_rust_string_lossy(s))
}

/// WebIDL `sequence<DOMString>` conversion.
pub(crate) fn string_sequence(
    s: &mut v8::PinScope<'_, '_>,
    v: v8::Local<'_, v8::Value>,
) -> NRes<Vec<String>> {
    const NOT_SEQ: &str = "The provided value cannot be converted to a sequence.";
    let Ok(obj) = v8::Local::<v8::Object>::try_from(v) else {
        return Err(NErr::Type(NOT_SEQ.into()));
    };
    let sym = v8::Symbol::get_iterator(s);
    let method = obj.get(s, sym.into()).ok_or(NErr::Thrown)?;
    let Ok(method) = v8::Local::<v8::Function>::try_from(method) else {
        return Err(NErr::Type(NOT_SEQ.into()));
    };
    let it = method.call(s, v, &[]).ok_or(NErr::Thrown)?;
    let Ok(it) = v8::Local::<v8::Object>::try_from(it) else {
        return Err(NErr::Type("The iterator is not an object.".into()));
    };
    let next_key = v8::String::new(s, "next").ok_or(NErr::Thrown)?;
    let next = it.get(s, next_key.into()).ok_or(NErr::Thrown)?;
    let Ok(next) = v8::Local::<v8::Function>::try_from(next) else {
        return Err(NErr::Type(NOT_SEQ.into()));
    };
    let done_key = v8::String::new(s, "done").ok_or(NErr::Thrown)?;
    let value_key = v8::String::new(s, "value").ok_or(NErr::Thrown)?;
    let mut out = Vec::new();
    loop {
        let r = next.call(s, it.into(), &[]).ok_or(NErr::Thrown)?;
        let Ok(r) = v8::Local::<v8::Object>::try_from(r) else {
            return Err(NErr::Type("The iterator result is not an object.".into()));
        };
        let done = r.get(s, done_key.into()).ok_or(NErr::Thrown)?;
        if done.boolean_value(s) {
            break;
        }
        let value = r.get(s, value_key.into()).ok_or(NErr::Thrown)?;
        out.push(to_string(s, value)?);
        if out.len() > 1 << 16 {
            break;
        }
    }
    Ok(out)
}

/// `obj[key]` if `obj` has `key` (Blink's dictionary reads).
fn get<'s>(
    s: &mut v8::PinScope<'s, '_>,
    o: v8::Local<'s, v8::Object>,
    key: &str,
) -> NRes<Option<v8::Local<'s, v8::Value>>> {
    let k = v8::String::new(s, key).ok_or(NErr::Thrown)?;
    match o.has(s, k.into()) {
        None => Err(NErr::Thrown),
        Some(false) => Ok(None),
        Some(true) => o.get(s, k.into()).map(Some).ok_or(NErr::Thrown),
    }
}

/// Bytes of an `ArrayBuffer` or `ArrayBufferView`.
pub(crate) fn bytes(s: &mut v8::PinScope<'_, '_>, v: v8::Local<'_, v8::Value>) -> Option<Vec<u8>> {
    if let Ok(view) = v8::Local::<v8::ArrayBufferView>::try_from(v) {
        let mut out = vec![0u8; view.byte_length()];
        let n = view.copy_contents(&mut out);
        out.truncate(n);
        return Some(out);
    }
    if let Ok(ab) = v8::Local::<v8::ArrayBuffer>::try_from(v) {
        let len = ab.byte_length();
        let view = v8::Uint8Array::new(s, ab, 0, len)?;
        let mut out = vec![0u8; len];
        let n = view.copy_contents(&mut out);
        out.truncate(n);
        return Some(out);
    }
    None
}

struct Reader<'a> {
    ctx: &'a str,
    dict: &'static str,
}

impl Reader<'_> {
    fn err(&self, key: &str, msg: &str) -> NErr {
        NErr::Type(format!("{}{}: {key}: {msg}", self.ctx, self.dict))
    }

    fn buffer<'s>(
        &self,
        s: &mut v8::PinScope<'s, '_>,
        o: v8::Local<'s, v8::Object>,
        key: &str,
        required: bool,
    ) -> NRes<Option<Vec<u8>>> {
        match get(s, o, key)? {
            None if required => Err(self.err(key, "Missing required property")),
            None => Ok(None),
            Some(v) => bytes(s, v)
                .map(Some)
                .ok_or_else(|| self.err(key, "Not a BufferSource")),
        }
    }

    fn integer<'s>(
        &self,
        s: &mut v8::PinScope<'s, '_>,
        o: v8::Local<'s, v8::Object>,
        key: &str,
        required: bool,
        max: f64,
    ) -> NRes<Option<u32>> {
        let Some(v) = get(s, o, key)? else {
            return if required {
                Err(self.err(key, "Missing required property"))
            } else {
                Ok(None)
            };
        };
        let n = v.number_value(s).ok_or(NErr::Thrown)?;
        if n.is_nan() {
            return Err(self.err(key, "Is not a number"));
        }
        let n = n.trunc();
        if n.is_infinite() || n < 0.0 || n > max {
            return Err(self.err(key, "Outside of numeric range"));
        }
        Ok(Some(n as u32))
    }

    fn hash<'s>(&self, s: &mut v8::PinScope<'s, '_>, o: v8::Local<'s, v8::Object>) -> NRes<Alg> {
        match get(s, o, "hash")? {
            Some(v) if v.is_string() || v.is_object() => {
                let sub = format!("{}{}: hash: ", self.ctx, self.dict);
                Ok(normalize(s, v, Op::Digest, &sub)?.params.alg)
            }
            _ => Err(self.err("hash", "Missing or not an AlgorithmIdentifier")),
        }
    }

    fn curve<'s>(&self, s: &mut v8::PinScope<'s, '_>, o: v8::Local<'s, v8::Object>) -> NRes<Curve> {
        match get(s, o, "namedCurve")? {
            Some(v) if v.is_string() => {
                let name = to_string(s, v)?;
                Curve::from_name(&name).ok_or_else(|| {
                    NErr::NotSupported(format!(
                        "{}{}: Unrecognized namedCurve",
                        self.ctx, self.dict
                    ))
                })
            }
            _ => Err(self.err("namedCurve", "Missing or not a string")),
        }
    }
}

const OCTET: f64 = 255.0;
const USHORT: f64 = 65535.0;
const ULONG: f64 = 4294967295.0;

/// Normalize an `AlgorithmIdentifier` (object or string) for `op`.
/// `ctx` prefixes errors (nested `hash` members).
pub(crate) fn normalize<'s>(
    s: &mut v8::PinScope<'s, '_>,
    v: v8::Local<'s, v8::Value>,
    op: Op,
    ctx: &str,
) -> NRes<Normalized> {
    let Ok(obj) = v8::Local::<v8::Object>::try_from(v) else {
        let name = to_string(s, v)?;
        return normalize_name(s, &name, None, op, ctx);
    };
    // The `Algorithm` dictionary: `required DOMString name`.
    let key = v8::String::new(s, "name").ok_or(NErr::Thrown)?;
    let n = obj.get(s, key.into()).ok_or(NErr::Thrown)?;
    let name = match n.is_undefined() {
        false => to_string(s, n)?,
        true => {
            return Err(NErr::Type(format!(
                "{ctx}Algorithm: name: Missing or not a string"
            )));
        }
    };
    normalize_name(s, &name, Some(obj), op, ctx)
}

/// Normalize an algorithm given by name, reading its parameters from
/// `dict` (an empty dictionary when `None`).
pub(crate) fn normalize_name(
    s: &mut v8::PinScope<'_, '_>,
    name: &str,
    dict: Option<v8::Local<'_, v8::Object>>,
    op: Op,
    ctx: &str,
) -> NRes<Normalized> {
    let Some(alg) = Alg::from_name(name) else {
        return Err(NErr::NotSupported(format!(
            "{ctx}Algorithm: Unrecognized name"
        )));
    };
    let Some(d) = dict_for(alg, op) else {
        return Err(NErr::NotSupported(format!(
            "{ctx}{}: Unsupported operation: {}",
            alg.name(),
            op.name()
        )));
    };
    let mut p = Params {
        alg,
        ..Params::default()
    };
    if d == Dict::None {
        return Ok(Normalized {
            params: p,
            public: None,
        });
    }
    let o = match dict {
        Some(o) => v8::Local::new(s, o),
        None => v8::Object::new(s),
    };
    let r = Reader {
        ctx,
        dict: d.name(),
    };
    let mut public = None;
    match d {
        Dict::None => {}
        Dict::AesCbc => p.iv = r.buffer(s, o, "iv", true)?,
        Dict::Aead => {
            p.iv = r.buffer(s, o, "iv", true)?;
            p.additional_data = r.buffer(s, o, "additionalData", false)?;
            p.tag_length = r.integer(s, o, "tagLength", false, OCTET)?;
        }
        Dict::AesCtr => {
            p.counter = r.buffer(s, o, "counter", true)?;
            p.length = r.integer(s, o, "length", true, OCTET)?;
        }
        Dict::AesKeyGen | Dict::AesDerived => {
            p.length = r.integer(s, o, "length", true, USHORT)?;
        }
        Dict::HmacImport | Dict::HmacKeyGen => {
            p.hash = Some(r.hash(s, o)?);
            p.length = r.integer(s, o, "length", false, ULONG)?;
        }
        Dict::RsaHashedKeyGen => {
            p.modulus_length = r.integer(s, o, "modulusLength", true, ULONG)?.unwrap_or(0);
            p.public_exponent = match get(s, o, "publicExponent")? {
                Some(v) if v.is_uint8_array() => bytes(s, v).unwrap_or_default(),
                _ => return Err(r.err("publicExponent", "Missing or not a Uint8Array")),
            };
            p.hash = Some(r.hash(s, o)?);
        }
        Dict::RsaHashedImport | Dict::Ecdsa => p.hash = Some(r.hash(s, o)?),
        Dict::RsaPss => {
            p.salt_length = r.integer(s, o, "saltLength", true, ULONG)?.unwrap_or(0);
        }
        Dict::RsaOaep => p.label = r.buffer(s, o, "label", false)?,
        Dict::EcKeyGen | Dict::EcKeyImport => p.curve = Some(r.curve(s, o)?),
        Dict::EcdhDerive => match get(s, o, "public")? {
            None => return Err(r.err("public", "Missing required property")),
            Some(v) => public = Some(v8::Global::new(s, v)),
        },
        Dict::Hkdf => {
            p.hash = Some(r.hash(s, o)?);
            p.salt = r.buffer(s, o, "salt", true)?;
            p.info = r.buffer(s, o, "info", true)?;
        }
        Dict::Pbkdf2 => {
            p.salt = r.buffer(s, o, "salt", true)?;
            p.iterations = r.integer(s, o, "iterations", true, ULONG)?.unwrap_or(0);
            p.hash = Some(r.hash(s, o)?);
        }
    }
    Ok(Normalized { params: p, public })
}

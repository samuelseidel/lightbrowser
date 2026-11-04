//! JSON Web Keys as Chromium's `components/webcrypto/jwk.cc`: the
//! `JsonWebKey` dictionary (WebIDL conversion of its members), the
//! checks of `JwkReader::Init` in order, and exports with the members
//! sorted by name (Chromium writes a `base::Value` dictionary).

use std::collections::BTreeMap;

use boring::bn::BigNum;
use boring::ec::{EcGroup, EcKey};
use boring::pkey::{Id, PKey, Private, Public};
use boring::rsa::Rsa;
use lb_js::v8;

use super::normalize::{Alg, NErr, NRes, Params};
use super::ops::{self, CryptoKey, Material};
use super::{DECRYPT, ENCRYPT, Fail, SIGN, UNWRAP, USAGES, VERIFY, WRAP, usage_names};

/// The members of a `JsonWebKey` this implementation reads.
#[derive(Clone, Debug, Default)]
pub(crate) struct Jwk {
    pub kty: Option<String>,
    pub use_: Option<String>,
    pub key_ops: Option<Vec<String>>,
    pub alg: Option<String>,
    pub ext: Option<bool>,
    pub crv: Option<String>,
    pub x: Option<String>,
    pub y: Option<String>,
    pub d: Option<String>,
    pub n: Option<String>,
    pub e: Option<String>,
    pub p: Option<String>,
    pub q: Option<String>,
    pub dp: Option<String>,
    pub dq: Option<String>,
    pub qi: Option<String>,
    pub k: Option<String>,
}

/// WebIDL conversion of a `JsonWebKey` dictionary (members in
/// lexicographic order; `undefined` is absent).
pub(crate) fn read_dict(s: &mut v8::PinScope<'_, '_>, v: v8::Local<'_, v8::Value>) -> NRes<Jwk> {
    let Ok(o) = v8::Local::<v8::Object>::try_from(v) else {
        return Ok(Jwk::default());
    };
    let mut j = Jwk::default();
    let member = |s: &mut v8::PinScope<'_, '_>, key: &str| -> NRes<Option<v8::Global<v8::Value>>> {
        let k = v8::String::new(s, key).ok_or(NErr::Thrown)?;
        let val = o.get(s, k.into()).ok_or(NErr::Thrown)?;
        Ok((!val.is_undefined()).then(|| v8::Global::new(s, val)))
    };
    macro_rules! string {
        ($key:literal, $field:ident) => {
            if let Some(g) = member(s, $key)? {
                let val = v8::Local::new(s, &g);
                j.$field = Some(super::normalize::to_string(s, val)?);
            }
        };
    }
    string!("alg", alg);
    string!("crv", crv);
    string!("d", d);
    string!("dp", dp);
    string!("dq", dq);
    string!("e", e);
    if let Some(g) = member(s, "ext")? {
        let val = v8::Local::new(s, &g);
        j.ext = Some(val.boolean_value(s));
    }
    string!("k", k);
    if let Some(g) = member(s, "key_ops")? {
        let val = v8::Local::new(s, &g);
        j.key_ops = Some(super::normalize::string_sequence(s, val)?);
    }
    string!("kty", kty);
    string!("n", n);
    string!("p", p);
    string!("q", q);
    string!("qi", qi);
    string!("use", use_);
    string!("x", x);
    string!("y", y);
    Ok(j)
}

/// A JWK from JSON bytes (`unwrapKey` with format `jwk`).
pub(crate) fn parse_json(bytes: &[u8]) -> Result<Jwk, Fail> {
    let not_dict = || Fail::data("JWK input could not be parsed to a JSON dictionary");
    let v: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| not_dict())?;
    let o = v.as_object().ok_or_else(not_dict)?;
    let s = |k: &str| o.get(k).and_then(|v| v.as_str()).map(str::to_owned);
    Ok(Jwk {
        kty: s("kty"),
        use_: s("use"),
        key_ops: o.get("key_ops").and_then(|v| v.as_array()).map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        }),
        alg: s("alg"),
        ext: o.get("ext").and_then(|v| v.as_bool()),
        crv: s("crv"),
        x: s("x"),
        y: s("y"),
        d: s("d"),
        n: s("n"),
        e: s("e"),
        p: s("p"),
        q: s("q"),
        dp: s("dp"),
        dq: s("dq"),
        qi: s("qi"),
        k: s("k"),
    })
}

fn missing(m: &str) -> Fail {
    Fail::data(format!("The required JWK member \"{m}\" was missing"))
}

const ENC_USAGE: u8 = ENCRYPT | DECRYPT | WRAP | UNWRAP;
const SIG_USAGE: u8 = SIGN | VERIFY;

impl Jwk {
    /// `JwkReader::Init`: kty, ext, key_ops, use, both, alg.
    fn init(&self, extractable: bool, usages: u8, kty: &str, alg: &str) -> Result<(), Fail> {
        match &self.kty {
            None => return Err(missing("kty")),
            Some(k) if k != kty => {
                return Err(Fail::data(format!(
                    "The JWK \"kty\" member was not \"{kty}\""
                )));
            }
            _ => {}
        }
        if self.ext == Some(false) && extractable {
            return Err(Fail::data(
                "The \"ext\" member of the JWK dictionary is inconsistent what that specified by the Web Crypto call",
            ));
        }
        let mut ops_mask = 0u8;
        if let Some(ops) = &self.key_ops {
            for op in ops {
                if let Some(i) = USAGES.iter().position(|u| u == op) {
                    if ops_mask & (1 << i) != 0 {
                        return Err(Fail::data(
                            "The \"key_ops\" member of the JWK dictionary contains duplicate usages.",
                        ));
                    }
                    ops_mask |= 1 << i;
                }
            }
            if usages & !ops_mask != 0 {
                return Err(Fail::data(
                    "The JWK \"key_ops\" member was inconsistent with that specified by the Web Crypto call. The JWK usage must be a superset of those requested",
                ));
            }
        }
        let mut use_mask = 0u8;
        if let Some(u) = &self.use_ {
            use_mask = match u.as_str() {
                "enc" => ENC_USAGE,
                "sig" => SIG_USAGE,
                _ => return Err(Fail::data("The JWK \"use\" member could not be parsed")),
            };
            if usages & !use_mask != 0 {
                return Err(Fail::data(
                    "The JWK \"use\" member was inconsistent with that specified by the Web Crypto call. The JWK usage must be a superset of those requested",
                ));
            }
        }
        if self.key_ops.is_some() && self.use_.is_some() && ops_mask & !use_mask != 0 {
            return Err(Fail::data(
                "The JWK \"use\" and \"key_ops\" properties were both found but are inconsistent with each other.",
            ));
        }
        if let Some(a) = &self.alg
            && !alg.is_empty()
            && a != alg
        {
            return Err(alg_inconsistent());
        }
        Ok(())
    }

    /// The `k` of an `oct` key after `init`.
    pub(crate) fn read_secret(
        &self,
        extractable: bool,
        usages: u8,
        alg: &str,
    ) -> Result<Vec<u8>, Fail> {
        self.init(extractable, usages, "oct", alg)?;
        bytes(&self.k, "k")
    }
}

fn alg_inconsistent() -> Fail {
    Fail::data("The JWK \"alg\" member was inconsistent with that specified by the Web Crypto call")
}

/// A base64url member as bytes.
fn bytes(v: &Option<String>, name: &str) -> Result<Vec<u8>, Fail> {
    let Some(s) = v else {
        return Err(missing(name));
    };
    b64url_decode(s).ok_or_else(|| {
        Fail::data(format!(
            "The JWK member \"{name}\" could not be base64url decoded or contained padding"
        ))
    })
}

/// A big-endian integer member (`n`, `e`, `d`, ...).
fn big(v: &Option<String>, name: &str) -> Result<BigNum, Fail> {
    let b = bytes(v, name)?;
    if b.is_empty() {
        return Err(Fail::data(format!("The JWK \"{name}\" member was empty.")));
    }
    if b[0] == 0 {
        return Err(Fail::data(format!(
            "The JWK \"{name}\" member contained a leading zero."
        )));
    }
    BigNum::from_slice(&b).map_err(|_| Fail::op(""))
}

/// AES `alg` check of Chromium's `ImportKeyJwk` for AES.
pub(crate) fn check_aes_alg(j: &Jwk, a: Alg, len: usize) -> Result<(), Fail> {
    let Some(alg) = &j.alg else { return Ok(()) };
    if *alg != aes_alg_name(a, len) {
        if [16, 24, 32].iter().any(|l| *alg == aes_alg_name(a, *l)) {
            return Err(Fail::data(
                "The JWK \"k\" member did not include the right length of key data for the given algorithm.",
            ));
        }
        return Err(alg_inconsistent());
    }
    Ok(())
}

pub(crate) fn aes_alg_name(a: Alg, len: usize) -> String {
    let bits = match len {
        16 => "128",
        24 => "192",
        32 => "256",
        _ => return String::new(),
    };
    format!("A{bits}{}", ops::aes_jwk_suffix(a))
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Base64url without padding.
pub(crate) fn b64url(b: &[u8]) -> String {
    let mut out = String::with_capacity(b.len().div_ceil(3) * 4);
    for c in b.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        let chars = c.len() + 1;
        for i in 0..chars {
            out.push(B64[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

fn b64url_decode(s: &str) -> Option<Vec<u8>> {
    if s.contains('=') {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc = 0u32;
    let mut n = 0;
    for ch in s.bytes() {
        let v = B64.iter().position(|x| *x == ch)? as u32;
        acc = (acc << 6) | v;
        n += 1;
        if n == 4 {
            out.extend_from_slice(&[(acc >> 16) as u8, (acc >> 8) as u8, acc as u8]);
            acc = 0;
            n = 0;
        }
    }
    match n {
        0 => {}
        2 => out.push((acc >> 4) as u8),
        3 => out.extend_from_slice(&[(acc >> 10) as u8, (acc >> 2) as u8]),
        _ => return None,
    }
    Some(out)
}

/// A JWK being written: members sorted by name.
pub(crate) struct JwkOut(BTreeMap<&'static str, String>);

impl JwkOut {
    pub(crate) fn new(kty: &str, key: &CryptoKey) -> JwkOut {
        let mut m = BTreeMap::new();
        m.insert("kty", json_str(kty));
        m.insert("ext", key.extractable.to_string());
        let ops: Vec<String> = usage_names(key.usages).into_iter().map(json_str).collect();
        m.insert("key_ops", format!("[{}]", ops.join(",")));
        JwkOut(m)
    }
    pub(crate) fn str(&mut self, k: &'static str, v: &str) {
        self.0.insert(k, json_str(v));
    }
    pub(crate) fn finish(self) -> String {
        let parts: Vec<String> = self
            .0
            .into_iter()
            .map(|(k, v)| format!("{}:{v}", json_str(k)))
            .collect();
        format!("{{{}}}", parts.join(","))
    }
}

fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_default()
}

fn padded(b: &boring::bn::BigNumRef, n: usize) -> Result<String, Fail> {
    Ok(b64url(&b.to_vec_padded(n).map_err(|_| Fail::op(""))?))
}

pub(crate) fn export_public(pk: &PKey<Public>, key: &CryptoKey) -> Result<String, Fail> {
    let a = key.alg.id;
    match a {
        Alg::Ecdsa | Alg::Ecdh => {
            let c = key.alg.curve.unwrap_or(super::normalize::Curve::P256);
            let n = ops::curve_bytes(c);
            let ec = pk.ec_key().map_err(|_| Fail::op(""))?;
            let (x, y) = ec_xy(&ec)?;
            let mut o = JwkOut::new("EC", key);
            o.str("crv", c.name());
            o.str("x", &padded(&x, n)?);
            o.str("y", &padded(&y, n)?);
            Ok(o.finish())
        }
        Alg::RsaPkcs1 | Alg::RsaPss | Alg::RsaOaep => {
            let rsa = pk.rsa().map_err(|_| Fail::op(""))?;
            let mut o = JwkOut::new("RSA", key);
            o.str(
                "alg",
                ops::rsa_jwk_alg(a, key.alg.hash.unwrap_or(Alg::Sha256)),
            );
            o.str("n", &b64url(&rsa.n().to_vec()));
            o.str("e", &b64url(&rsa.e().to_vec()));
            Ok(o.finish())
        }
        _ => {
            let raw = ops::public_raw(pk, a)?;
            let mut o = JwkOut::new("OKP", key);
            o.str("crv", a.name());
            o.str("x", &b64url(&raw));
            Ok(o.finish())
        }
    }
}

fn ec_xy<T>(ec: &EcKey<T>) -> Result<(BigNum, BigNum), Fail>
where
    T: boring::pkey::HasPublic,
{
    let mut x = BigNum::new().map_err(|_| Fail::op(""))?;
    let mut y = BigNum::new().map_err(|_| Fail::op(""))?;
    let mut ctx = boring::bn::BigNumContext::new().map_err(|_| Fail::op(""))?;
    ec.public_key()
        .affine_coordinates_gfp(ec.group(), &mut x, &mut y, &mut ctx)
        .map_err(|_| Fail::op(""))?;
    Ok((x, y))
}

pub(crate) fn export_private(sk: &PKey<Private>, key: &CryptoKey) -> Result<String, Fail> {
    let a = key.alg.id;
    match a {
        Alg::Ecdsa | Alg::Ecdh => {
            let c = key.alg.curve.unwrap_or(super::normalize::Curve::P256);
            let n = ops::curve_bytes(c);
            let ec = sk.ec_key().map_err(|_| Fail::op(""))?;
            let (x, y) = ec_xy(&ec)?;
            let mut o = JwkOut::new("EC", key);
            o.str("crv", c.name());
            o.str("x", &padded(&x, n)?);
            o.str("y", &padded(&y, n)?);
            o.str("d", &padded(ec.private_key(), n)?);
            Ok(o.finish())
        }
        Alg::RsaPkcs1 | Alg::RsaPss | Alg::RsaOaep => {
            let rsa = sk.rsa().map_err(|_| Fail::op(""))?;
            let mut o = JwkOut::new("RSA", key);
            o.str(
                "alg",
                ops::rsa_jwk_alg(a, key.alg.hash.unwrap_or(Alg::Sha256)),
            );
            o.str("n", &b64url(&rsa.n().to_vec()));
            o.str("e", &b64url(&rsa.e().to_vec()));
            o.str("d", &b64url(&rsa.d().to_vec()));
            let part = |b: Option<&boring::bn::BigNumRef>| b.map(|b| b64url(&b.to_vec()));
            for (k, v) in [
                ("p", part(rsa.p())),
                ("q", part(rsa.q())),
                ("dp", part(rsa.dmp1())),
                ("dq", part(rsa.dmq1())),
                ("qi", part(rsa.iqmp())),
            ] {
                o.str(k, &v.ok_or_else(|| Fail::op(""))?);
            }
            Ok(o.finish())
        }
        _ => {
            let mut d = [0u8; 32];
            let n = sk.raw_private_key(&mut d).map_err(|_| Fail::op(""))?.len();
            let pk = sk.public_key_to_der().map_err(|_| Fail::op(""))?;
            let pk = PKey::public_key_from_der(&pk).map_err(|_| Fail::op(""))?;
            let raw = ops::public_raw(&pk, a)?;
            let mut o = JwkOut::new("OKP", key);
            o.str("crv", a.name());
            o.str("x", &b64url(&raw));
            o.str("d", &b64url(&d[..n]));
            Ok(o.finish())
        }
    }
}

/// Import an `EC`, `RSA` or `OKP` JWK.
pub(crate) fn import_asym(
    j: &Jwk,
    p: &Params,
    extractable: bool,
    usages: u8,
) -> Result<Material, Fail> {
    let a = p.alg;
    let (pub_all, priv_all) = ops::asym_usage_sets(a);
    match a {
        Alg::Ecdsa | Alg::Ecdh => {
            j.init(extractable, usages, "EC", "")?;
            let Some(crv) = &j.crv else {
                return Err(missing("crv"));
            };
            let want = p.curve.unwrap_or(super::normalize::Curve::P256);
            if super::normalize::Curve::from_name(crv) != Some(want) {
                return Err(Fail::data(
                    "The JWK's \"crv\" member specifies a different curve than requested",
                ));
            }
            let private = j.d.is_some();
            let allowed = if private { priv_all } else { pub_all };
            if usages & !allowed != 0 {
                return Err(Fail::syntax(
                    "Cannot create a key using the specified key usages.",
                ));
            }
            let n = ops::curve_bytes(want);
            let fixed = |v: &Option<String>, name: &str| -> Result<Vec<u8>, Fail> {
                let b = bytes(v, name)?;
                if b.len() != n {
                    return Err(Fail::data(format!(
                        "The JWK's \"{name}\" member defines an octet string of length {} bytes but should be {n}",
                        b.len()
                    )));
                }
                Ok(b)
            };
            let x = fixed(&j.x, "x")?;
            let y = fixed(&j.y, "y")?;
            let g = EcGroup::from_curve_name(ops::nid_for_curve(want)).map_err(|_| Fail::op(""))?;
            let (xb, yb) = (
                BigNum::from_slice(&x).map_err(|_| Fail::op(""))?,
                BigNum::from_slice(&y).map_err(|_| Fail::op(""))?,
            );
            let public = EcKey::from_public_key_affine_coordinates(&g, &xb, &yb)
                .map_err(|_| Fail::data("The imported EC key is invalid"))?;
            if private {
                let d = fixed(&j.d, "d")?;
                let d = BigNum::from_slice(&d).map_err(|_| Fail::op(""))?;
                let ec = EcKey::from_private_components(&g, &d, public.public_key())
                    .map_err(|_| Fail::data("The imported EC key is invalid"))?;
                if ec.check_key().is_err() {
                    return Err(Fail::data("The imported EC key is invalid"));
                }
                Ok(Material::Private(
                    PKey::from_ec_key(ec).map_err(|_| Fail::op(""))?,
                ))
            } else {
                if public.check_key().is_err() {
                    return Err(Fail::data("The imported EC key is invalid"));
                }
                Ok(Material::Public(
                    PKey::from_ec_key(public).map_err(|_| Fail::op(""))?,
                ))
            }
        }
        Alg::RsaPkcs1 | Alg::RsaPss | Alg::RsaOaep => {
            let alg = ops::rsa_jwk_alg(a, p.hash.unwrap_or(Alg::Sha256));
            j.init(extractable, usages, "RSA", alg)?;
            let private = j.d.is_some();
            let allowed = if private { priv_all } else { pub_all };
            if usages & !allowed != 0 {
                return Err(Fail::syntax(
                    "Cannot create a key using the specified key usages.",
                ));
            }
            let n = big(&j.n, "n")?;
            let e = big(&j.e, "e")?;
            if !private {
                let rsa = Rsa::from_public_components(n, e).map_err(|_| Fail::data(""))?;
                return Ok(Material::Public(
                    PKey::from_rsa(rsa).map_err(|_| Fail::op(""))?,
                ));
            }
            let d = big(&j.d, "d")?;
            let pp = big(&j.p, "p")?;
            let q = big(&j.q, "q")?;
            let dp = big(&j.dp, "dp")?;
            let dq = big(&j.dq, "dq")?;
            let qi = big(&j.qi, "qi")?;
            let rsa = Rsa::from_private_components(n, e, d, pp, q, dp, dq, qi)
                .map_err(|_| Fail::data(""))?;
            if !rsa.check_key().unwrap_or(false) {
                return Err(Fail::data(""));
            }
            Ok(Material::Private(
                PKey::from_rsa(rsa).map_err(|_| Fail::op(""))?,
            ))
        }
        _ => {
            j.init(extractable, usages, "OKP", "")?;
            let Some(crv) = &j.crv else {
                return Err(missing("crv"));
            };
            if crv != a.name() {
                return Err(Fail::data(
                    "The JWK's \"crv\" member specifies a different curve than requested",
                ));
            }
            let id = if a == Alg::Ed25519 {
                Id::ED25519
            } else {
                Id::X25519
            };
            let private = j.d.is_some();
            let allowed = if private { priv_all } else { pub_all };
            if usages & !allowed != 0 {
                return Err(Fail::syntax(
                    "Cannot create a key using the specified key usages.",
                ));
            }
            if private {
                let d = bytes(&j.d, "d")?;
                Ok(Material::Private(ops::raw_okp_private(id, &d)?))
            } else {
                let x = bytes(&j.x, "x")?;
                Ok(Material::Public(ops::raw_okp_from_public(id, &x)?))
            }
        }
    }
}

//! The Web Crypto operations on BoringSSL, with the checks and status
//! texts of Chromium's `components/webcrypto` (`status.cc`).

use boring::bn::{BigNum, BigNumContext};
use boring::ec::{EcGroup, EcKey, EcPoint, PointConversionForm};
use boring::ecdsa::EcdsaSig;
use boring::hash::MessageDigest;
use boring::nid::Nid;
use boring::pkey::{Id, PKey, Private, Public};
use boring::rsa::{Padding, Rsa};
use boring::sign::{RsaPssSaltlen, Signer, Verifier};

use super::jwk::{self, JwkOut};
use super::normalize::{Alg, Curve, Params};
use super::{
    DECRYPT, DERIVE_BITS, DERIVE_KEY, ENCRYPT, Fail, Format, KeyData, SIGN, UNWRAP, VERIFY, WRAP,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum KeyType {
    Secret,
    Public,
    Private,
}

impl KeyType {
    pub(crate) fn name(self) -> &'static str {
        match self {
            KeyType::Secret => "secret",
            KeyType::Public => "public",
            KeyType::Private => "private",
        }
    }
}

/// `CryptoKey.algorithm` as data.
#[derive(Clone, Debug, Default)]
pub(crate) struct KeyAlg {
    pub id: Alg,
    pub hash: Option<Alg>,
    pub length: u32,
    pub curve: Option<Curve>,
    pub modulus: u32,
    pub exponent: Vec<u8>,
}

pub(crate) enum Material {
    Secret(Vec<u8>),
    Private(PKey<Private>),
    Public(PKey<Public>),
}

/// A key: immutable, shared by the objects that hold it (`Arc`: keys
/// travel to workers by structured clone).
pub(crate) struct CryptoKey {
    pub ty: KeyType,
    pub extractable: bool,
    pub alg: KeyAlg,
    pub usages: u8,
    pub mat: Material,
}

impl CryptoKey {
    fn secret(&self) -> &[u8] {
        match &self.mat {
            Material::Secret(b) => b,
            _ => &[],
        }
    }
}

pub(crate) enum Generated {
    Secret(CryptoKey),
    Pair(CryptoKey, CryptoKey),
}

pub(crate) enum Exported {
    Bytes(Vec<u8>),
    Jwk(String),
}

/// Random bytes from BoringSSL (the OS CSPRNG).
pub(crate) fn random(buf: &mut [u8]) {
    if boring::rand::rand_bytes(buf).is_err() {
        // RAND_bytes aborts rather than fail in BoringSSL; keep zeros
        // out anyway.
        for (i, b) in buf.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(151);
        }
    }
}

fn md(alg: Alg) -> MessageDigest {
    match alg {
        Alg::Sha1 => MessageDigest::sha1(),
        Alg::Sha384 => MessageDigest::sha384(),
        Alg::Sha512 => MessageDigest::sha512(),
        _ => MessageDigest::sha256(),
    }
}

/// HMAC block size in bits (the default key length).
fn block_bits(hash: Alg) -> u32 {
    match hash {
        Alg::Sha384 | Alg::Sha512 => 1024,
        _ => 512,
    }
}

pub(crate) fn digest(alg: Alg, data: &[u8]) -> Vec<u8> {
    boring::hash::hash(md(alg), data)
        .map(|d| d.to_vec())
        .unwrap_or_default()
}

fn unexpected() -> Fail {
    Fail::op("")
}

fn contains(all: u8, usages: u8) -> bool {
    usages & !all == 0
}

fn bad_usages() -> Fail {
    Fail::syntax("Cannot create a key using the specified key usages.")
}

fn empty_usages() -> Fail {
    Fail::syntax("Usages cannot be empty when creating a key.")
}

/// `CheckKeyCreationUsages` for secret keys (usages must be allowed and
/// not empty).
fn check_secret_usages(all: u8, usages: u8) -> Result<(), Fail> {
    if !contains(all, usages) {
        return Err(bad_usages());
    }
    if usages == 0 {
        return Err(empty_usages());
    }
    Ok(())
}

/// Usages of the public and private halves of an asymmetric algorithm.
fn asym_usages(alg: Alg) -> (u8, u8) {
    match alg {
        Alg::RsaPkcs1 | Alg::RsaPss | Alg::Ecdsa | Alg::Ed25519 => (VERIFY, SIGN),
        Alg::RsaOaep => (ENCRYPT | WRAP, DECRYPT | UNWRAP),
        _ => (0, DERIVE_KEY | DERIVE_BITS),
    }
}

fn secret_usages(alg: Alg) -> u8 {
    match alg {
        Alg::AesKw => WRAP | UNWRAP,
        Alg::Hmac => SIGN | VERIFY,
        Alg::Hkdf | Alg::Pbkdf2 => DERIVE_KEY | DERIVE_BITS,
        _ => ENCRYPT | DECRYPT | WRAP | UNWRAP,
    }
}

fn is_aes(alg: Alg) -> bool {
    matches!(alg, Alg::AesGcm | Alg::AesCbc | Alg::AesCtr | Alg::AesKw)
}

fn is_rsa(alg: Alg) -> bool {
    matches!(alg, Alg::RsaPkcs1 | Alg::RsaPss | Alg::RsaOaep)
}

// ---------------------------------------------------------------------------
// Encrypt / decrypt / wrap
// ---------------------------------------------------------------------------

pub(crate) fn encrypt(p: &Params, key: &CryptoKey, data: &[u8]) -> Result<Vec<u8>, Fail> {
    match p.alg {
        Alg::AesGcm => aes_gcm(p, key.secret(), data, true),
        Alg::AesCbc => aes_cbc(p, key.secret(), data, true),
        Alg::AesCtr => aes_ctr(p, key.secret(), data),
        Alg::RsaOaep => rsa_oaep(p, key, data, true),
        _ => Err(unexpected()),
    }
}

pub(crate) fn decrypt(p: &Params, key: &CryptoKey, data: &[u8]) -> Result<Vec<u8>, Fail> {
    match p.alg {
        Alg::AesGcm => aes_gcm(p, key.secret(), data, false),
        Alg::AesCbc => aes_cbc(p, key.secret(), data, false),
        Alg::AesCtr => aes_ctr(p, key.secret(), data),
        Alg::RsaOaep => rsa_oaep(p, key, data, false),
        _ => Err(unexpected()),
    }
}

pub(crate) fn wrap(p: &Params, key: &CryptoKey, data: &[u8]) -> Result<Vec<u8>, Fail> {
    if p.alg == Alg::AesKw {
        if data.len() < 16 || !data.len().is_multiple_of(8) {
            return Err(Fail::data(
                "The AES-KW input data length is invalid: not a multiple of 8 bytes",
            ));
        }
        let k = boring::aes::AesKey::new_encrypt(key.secret()).map_err(|_| unexpected())?;
        let mut out = vec![0u8; data.len() + 8];
        boring::aes::wrap_key(&k, None, &mut out, data).map_err(|_| unexpected())?;
        return Ok(out);
    }
    encrypt(p, key, data)
}

pub(crate) fn unwrap(p: &Params, key: &CryptoKey, data: &[u8]) -> Result<Vec<u8>, Fail> {
    if p.alg == Alg::AesKw {
        if data.len() < 24 || !data.len().is_multiple_of(8) {
            return Err(Fail::data(
                "The AES-KW input data length is invalid: not a multiple of 8 bytes",
            ));
        }
        let k = boring::aes::AesKey::new_decrypt(key.secret()).map_err(|_| unexpected())?;
        let mut out = vec![0u8; data.len() - 8];
        boring::aes::unwrap_key(&k, None, &mut out, data).map_err(|_| unexpected())?;
        return Ok(out);
    }
    decrypt(p, key, data)
}

fn aes_gcm(p: &Params, key: &[u8], data: &[u8], seal: bool) -> Result<Vec<u8>, Fail> {
    let tag_bits = p.tag_length.unwrap_or(128);
    if ![32, 64, 96, 104, 112, 120, 128].contains(&tag_bits) {
        return Err(Fail::op(
            "The tag length is invalid: Must be 32, 64, 96, 104, 112, 120, or 128 bits",
        ));
    }
    let tag_len = (tag_bits / 8) as usize;
    let alg = match key.len() {
        16 => boring::aead::Algorithm::aes_128_gcm(),
        32 => boring::aead::Algorithm::aes_256_gcm(),
        _ => return Err(unexpected()),
    };
    let iv = p.iv.as_deref().unwrap_or(&[]);
    let aad = p.additional_data.as_deref().unwrap_or(&[]);
    let mut ctx = boring::aead::AeadCtx::new(&alg, key, tag_len).map_err(|_| unexpected())?;
    if seal {
        let mut buf = data.to_vec();
        let mut tag = vec![0u8; tag_len];
        let n = ctx
            .seal_in_place_mut(iv, &mut buf, &mut tag, aad)
            .map_err(|_| unexpected())?
            .len();
        buf.extend_from_slice(&tag[..n]);
        Ok(buf)
    } else {
        if data.len() < tag_len {
            return Err(Fail::op("The provided data is too small"));
        }
        let (ct, tag) = data.split_at(data.len() - tag_len);
        let mut buf = ct.to_vec();
        ctx.open_in_place_mut(iv, &mut buf, tag, aad)
            .map_err(|_| unexpected())?;
        Ok(buf)
    }
}

fn aes_cbc(p: &Params, key: &[u8], data: &[u8], enc: bool) -> Result<Vec<u8>, Fail> {
    let iv = p.iv.as_deref().unwrap_or(&[]);
    if iv.len() != 16 {
        return Err(Fail::op(
            "The \"iv\" has an unexpected length -- must be 16 bytes",
        ));
    }
    let cipher = match key.len() {
        16 => boring::symm::Cipher::aes_128_cbc(),
        32 => boring::symm::Cipher::aes_256_cbc(),
        _ => return Err(unexpected()),
    };
    let r = if enc {
        boring::symm::encrypt(cipher, key, Some(iv), data)
    } else {
        boring::symm::decrypt(cipher, key, Some(iv), data)
    };
    r.map_err(|_| unexpected())
}

/// AES-CTR where only the low `length` bits of the counter block count
/// (and wrap), as the spec and Chromium do it.
fn aes_ctr(p: &Params, key: &[u8], data: &[u8]) -> Result<Vec<u8>, Fail> {
    let counter = p.counter.as_deref().unwrap_or(&[]);
    if counter.len() != 16 {
        return Err(Fail::op(
            "The \"counter\" has an unexpected length -- must be 16 bytes",
        ));
    }
    let bits = p.length.unwrap_or(0);
    if bits == 0 || bits > 128 {
        return Err(Fail::op("The \"length\" member must be >= 1 and <= 128"));
    }
    let blocks = data.len().div_ceil(16) as u128;
    if bits < 128 && blocks > (1u128 << bits) {
        return Err(Fail::op("The input is too large for the counter length."));
    }
    let mut ctr = u128::from_be_bytes(counter.try_into().map_err(|_| unexpected())?);
    let mask: u128 = if bits == 128 {
        u128::MAX
    } else {
        (1u128 << bits) - 1
    };
    // The counter blocks, then one ECB pass over them for the key stream.
    let mut blocks = Vec::with_capacity(data.len().div_ceil(16) * 16);
    for _ in 0..data.len().div_ceil(16) {
        blocks.extend_from_slice(&ctr.to_be_bytes());
        let low = (ctr & mask).wrapping_add(1) & mask;
        ctr = (ctr & !mask) | low;
    }
    let cipher = match key.len() {
        16 => boring::symm::Cipher::aes_128_ecb(),
        32 => boring::symm::Cipher::aes_256_ecb(),
        _ => return Err(unexpected()),
    };
    let mut c = boring::symm::Crypter::new(cipher, boring::symm::Mode::Encrypt, key, None)
        .map_err(|_| unexpected())?;
    c.pad(false);
    let mut ks = vec![0u8; blocks.len() + 16];
    let n = c.update(&blocks, &mut ks).map_err(|_| unexpected())?;
    ks.truncate(n);
    Ok(data.iter().zip(ks.iter()).map(|(a, b)| a ^ b).collect())
}

// ---------------------------------------------------------------------------
// Sign / verify
// ---------------------------------------------------------------------------

pub(crate) fn sign(p: &Params, key: &CryptoKey, data: &[u8]) -> Result<Vec<u8>, Fail> {
    match p.alg {
        Alg::Hmac => {
            let h = key.alg.hash.unwrap_or(Alg::Sha256);
            hmac(h, key.secret(), data)
        }
        Alg::RsaPkcs1 | Alg::RsaPss => {
            let Material::Private(pk) = &key.mat else {
                return Err(Fail::access("The key is not of the expected type"));
            };
            let h = key.alg.hash.unwrap_or(Alg::Sha256);
            let mut s = Signer::new(md(h), pk).map_err(|_| unexpected())?;
            if p.alg == Alg::RsaPss {
                s.set_rsa_padding(Padding::PKCS1_PSS)
                    .map_err(|_| unexpected())?;
                s.set_rsa_pss_saltlen(RsaPssSaltlen::custom(p.salt_length as i32))
                    .map_err(|_| unexpected())?;
                s.set_rsa_mgf1_md(md(h)).map_err(|_| unexpected())?;
            }
            s.update(data).map_err(|_| unexpected())?;
            s.sign_to_vec().map_err(|_| unexpected())
        }
        Alg::Ecdsa => {
            let Material::Private(pk) = &key.mat else {
                return Err(Fail::access("The key is not of the expected type"));
            };
            let ec = pk.ec_key().map_err(|_| unexpected())?;
            let h = digest(p.hash.unwrap_or(Alg::Sha256), data);
            let sig = EcdsaSig::sign(&h, &ec).map_err(|_| unexpected())?;
            let n = field_bytes(key.alg.curve.unwrap_or(Curve::P256));
            let mut out = sig.r().to_vec_padded(n).map_err(|_| unexpected())?;
            out.extend(sig.s().to_vec_padded(n).map_err(|_| unexpected())?);
            Ok(out)
        }
        Alg::Ed25519 => {
            let Material::Private(pk) = &key.mat else {
                return Err(Fail::access("The key is not of the expected type"));
            };
            let mut s = Signer::new_without_digest(pk).map_err(|_| unexpected())?;
            s.sign_oneshot_to_vec(data).map_err(|_| unexpected())
        }
        _ => Err(unexpected()),
    }
}

pub(crate) fn verify(p: &Params, key: &CryptoKey, sig: &[u8], data: &[u8]) -> Result<bool, Fail> {
    match p.alg {
        Alg::Hmac => {
            let h = key.alg.hash.unwrap_or(Alg::Sha256);
            let mac = hmac(h, key.secret(), data)?;
            Ok(mac.len() == sig.len() && boring::memcmp::eq(&mac, sig))
        }
        Alg::RsaPkcs1 | Alg::RsaPss => {
            let Material::Public(pk) = &key.mat else {
                return Err(Fail::access("The key is not of the expected type"));
            };
            let h = key.alg.hash.unwrap_or(Alg::Sha256);
            let mut v = Verifier::new(md(h), pk).map_err(|_| unexpected())?;
            if p.alg == Alg::RsaPss {
                v.set_rsa_padding(Padding::PKCS1_PSS)
                    .map_err(|_| unexpected())?;
                v.set_rsa_pss_saltlen(RsaPssSaltlen::custom(p.salt_length as i32))
                    .map_err(|_| unexpected())?;
                v.set_rsa_mgf1_md(md(h)).map_err(|_| unexpected())?;
            }
            v.update(data).map_err(|_| unexpected())?;
            Ok(v.verify(sig).unwrap_or(false))
        }
        Alg::Ecdsa => {
            let Material::Public(pk) = &key.mat else {
                return Err(Fail::access("The key is not of the expected type"));
            };
            let n = field_bytes(key.alg.curve.unwrap_or(Curve::P256));
            if sig.len() != 2 * n {
                return Ok(false);
            }
            let ec = pk.ec_key().map_err(|_| unexpected())?;
            let (r, s) = sig.split_at(n);
            let (Ok(r), Ok(s)) = (BigNum::from_slice(r), BigNum::from_slice(s)) else {
                return Ok(false);
            };
            let Ok(sig) = EcdsaSig::from_private_components(r, s) else {
                return Ok(false);
            };
            let h = digest(p.hash.unwrap_or(Alg::Sha256), data);
            Ok(sig.verify(&h, &ec).unwrap_or(false))
        }
        Alg::Ed25519 => {
            let Material::Public(pk) = &key.mat else {
                return Err(Fail::access("The key is not of the expected type"));
            };
            let mut v = Verifier::new_without_digest(pk).map_err(|_| unexpected())?;
            Ok(v.verify_oneshot(sig, data).unwrap_or(false))
        }
        _ => Err(unexpected()),
    }
}

fn hmac(h: Alg, key: &[u8], data: &[u8]) -> Result<Vec<u8>, Fail> {
    let mut m = boring::hmac::Hmac::init(key, &md(h)).map_err(|_| unexpected())?;
    m.update(data).map_err(|_| unexpected())?;
    m.finalize().map_err(|_| unexpected())
}

// ---------------------------------------------------------------------------
// Derivation
// ---------------------------------------------------------------------------

/// The key length `deriveKey` asks for (`get key length`).
pub(crate) fn key_length(p: &Params) -> Result<Option<u32>, Fail> {
    match p.alg {
        a if is_aes(a) => {
            let l = p.length.unwrap_or(0);
            match l {
                128 | 256 => Ok(Some(l)),
                192 => Err(Fail::op("192-bit AES keys are not supported")),
                _ => Err(Fail::op("AES key length must be 128 or 256 bits")),
            }
        }
        Alg::Hmac => match p.length {
            Some(0) => Err(Fail::type_hmac_zero()),
            Some(l) => Ok(Some(l)),
            None => Ok(Some(block_bits(p.hash.unwrap_or(Alg::Sha256)))),
        },
        _ => Ok(None),
    }
}

impl Fail {
    fn type_hmac_zero() -> Fail {
        Fail::Type("HMAC key length must not be zero".into())
    }
}

pub(crate) fn derive_bits(
    p: &Params,
    key: &CryptoKey,
    length: Option<u32>,
) -> Result<Vec<u8>, Fail> {
    match p.alg {
        Alg::Pbkdf2 => {
            let Some(bits) = length else {
                return Err(Fail::op(
                    "No length was specified for the PBKDF2 Derive Bits operation.",
                ));
            };
            if bits % 8 != 0 {
                return Err(Fail::op(
                    "Length for PBKDF2 key derivation must be a multiple of 8 bits.",
                ));
            }
            if p.iterations == 0 {
                return Err(Fail::op("PBKDF2 requires iterations > 0"));
            }
            if bits == 0 {
                return Ok(Vec::new());
            }
            let mut out = vec![0u8; (bits / 8) as usize];
            boring::pkcs5::pbkdf2_hmac(
                key.secret(),
                p.salt.as_deref().unwrap_or(&[]),
                p.iterations as usize,
                md(p.hash.unwrap_or(Alg::Sha256)),
                &mut out,
            )
            .map_err(|_| unexpected())?;
            Ok(out)
        }
        Alg::Hkdf => {
            let Some(bits) = length else {
                return Err(Fail::op(
                    "No length was specified for the HKDF Derive Bits operation.",
                ));
            };
            if bits % 8 != 0 {
                return Err(Fail::op(
                    "The length provided for HKDF is not a multiple of 8 bits.",
                ));
            }
            let h = md(p.hash.unwrap_or(Alg::Sha256));
            if bits as usize > 255 * h.size() * 8 {
                return Err(Fail::op("The length provided for HKDF is too large."));
            }
            let suite = boring::hkdf::HkdfSuite::new(h);
            let prk = suite
                .extract(p.salt.as_deref().unwrap_or(&[]), key.secret())
                .map_err(|_| unexpected())?;
            let mut out = vec![0u8; (bits / 8) as usize];
            if !out.is_empty() {
                suite
                    .expand(&prk, p.info.as_deref().unwrap_or(&[]), &mut out)
                    .map_err(|_| unexpected())?;
            }
            Ok(out)
        }
        Alg::Ecdh | Alg::X25519 => {
            let Some(public) = &p.public_key else {
                return Err(unexpected());
            };
            if public.ty != KeyType::Public {
                return Err(Fail::access(if p.alg == Alg::Ecdh {
                    "The public parameter for ECDH key derivation is not a public EC key"
                } else {
                    "The public parameter for X25519 key derivation is not a public X25519 key"
                }));
            }
            if public.alg.id != p.alg {
                return Err(Fail::access(if p.alg == Alg::Ecdh {
                    "The public parameter for ECDH key derivation must be for ECDH"
                } else {
                    "The public parameter for X25519 key derivation must be for X25519"
                }));
            }
            if p.alg == Alg::Ecdh && public.alg.curve != key.alg.curve {
                return Err(Fail::access(
                    "The public parameter for ECDH key derivation is for a different named curve",
                ));
            }
            let (Material::Private(sk), Material::Public(pk)) = (&key.mat, &public.mat) else {
                return Err(unexpected());
            };
            let mut d = boring::derive::Deriver::new(sk).map_err(|_| unexpected())?;
            d.set_peer(pk).map_err(|_| unexpected())?;
            let secret = d.derive_to_vec().map_err(|_| unexpected())?;
            let max = secret.len() as u32 * 8;
            let Some(bits) = length else {
                return Ok(secret);
            };
            if bits > max {
                let what = if p.alg == Alg::Ecdh { "ECDH" } else { "X25519" };
                return Err(Fail::op(format!(
                    "Length specified for {what} key derivation is too large. Maximum allowed is {max} bits"
                )));
            }
            Ok(truncate_bits(secret, bits))
        }
        _ => Err(unexpected()),
    }
}

/// The first `bits` bits (the last byte's spare bits zeroed).
fn truncate_bits(mut b: Vec<u8>, bits: u32) -> Vec<u8> {
    let bytes = bits.div_ceil(8) as usize;
    b.truncate(bytes);
    let rem = bits % 8;
    if rem != 0
        && let Some(last) = b.last_mut()
    {
        *last &= 0xffu8 << (8 - rem);
    }
    b
}

// ---------------------------------------------------------------------------
// Key generation
// ---------------------------------------------------------------------------

fn curve_nid(c: Curve) -> Nid {
    match c {
        Curve::P256 => Nid::X9_62_PRIME256V1,
        Curve::P384 => Nid::SECP384R1,
        Curve::P521 => Nid::SECP521R1,
    }
}

fn field_bytes(c: Curve) -> usize {
    match c {
        Curve::P256 => 32,
        Curve::P384 => 48,
        Curve::P521 => 66,
    }
}

fn secret_key(alg: KeyAlg, extractable: bool, usages: u8, bytes: Vec<u8>) -> CryptoKey {
    CryptoKey {
        ty: KeyType::Secret,
        extractable,
        alg,
        usages,
        mat: Material::Secret(bytes),
    }
}

pub(crate) fn generate(p: &Params, extractable: bool, usages: u8) -> Result<Generated, Fail> {
    let alg = p.alg;
    if is_aes(alg) || alg == Alg::Hmac {
        check_secret_usages(secret_usages(alg), usages)?;
        let bits = if alg == Alg::Hmac {
            let h = p.hash.unwrap_or(Alg::Sha256);
            match p.length {
                Some(0) => return Err(Fail::op("HMAC key length must not be zero")),
                Some(l) => l,
                None => block_bits(h),
            }
        } else {
            match p.length.unwrap_or(0) {
                l @ (128 | 256) => l,
                192 => return Err(Fail::op("192-bit AES keys are not supported")),
                _ => return Err(Fail::op("AES key length must be 128 or 256 bits")),
            }
        };
        let mut b = vec![0u8; bits.div_ceil(8) as usize];
        random(&mut b);
        let b = truncate_bits(b, bits);
        let ka = KeyAlg {
            id: alg,
            hash: if alg == Alg::Hmac { p.hash } else { None },
            length: bits,
            ..KeyAlg::default()
        };
        return Ok(Generated::Secret(secret_key(ka, extractable, usages, b)));
    }
    let (pub_all, priv_all) = asym_usages(alg);
    if !contains(pub_all | priv_all, usages) {
        return Err(bad_usages());
    }
    let priv_usages = usages & priv_all;
    let pub_usages = usages & pub_all;
    let (sk, ka) = match alg {
        Alg::Ecdsa | Alg::Ecdh => {
            let c = p.curve.unwrap_or(Curve::P256);
            if priv_usages == 0 {
                return Err(empty_usages());
            }
            let g = EcGroup::from_curve_name(curve_nid(c)).map_err(|_| unexpected())?;
            let ec = EcKey::generate(&g).map_err(|_| unexpected())?;
            (
                PKey::from_ec_key(ec).map_err(|_| unexpected())?,
                KeyAlg {
                    id: alg,
                    curve: Some(c),
                    ..KeyAlg::default()
                },
            )
        }
        Alg::RsaPkcs1 | Alg::RsaPss | Alg::RsaOaep => {
            let bits = p.modulus_length;
            if !bits.is_multiple_of(8) || !(256..=16384).contains(&bits) {
                return Err(Fail::op(
                    "The modulus length must be a multiple of 8 bits and >= 256 and <= 16384",
                ));
            }
            let e = exponent_value(&p.public_exponent);
            if e != Some(3) && e != Some(65537) {
                return Err(Fail::op("The \"publicExponent\" must be either 3 or 65537"));
            }
            if priv_usages == 0 {
                return Err(empty_usages());
            }
            let e = BigNum::from_u32(e.unwrap_or(65537) as u32).map_err(|_| unexpected())?;
            let rsa = Rsa::generate_with_e(bits, &e).map_err(|_| unexpected())?;
            let exponent = rsa.e().to_vec();
            (
                PKey::from_rsa(rsa).map_err(|_| unexpected())?,
                KeyAlg {
                    id: alg,
                    hash: p.hash,
                    modulus: bits,
                    exponent,
                    ..KeyAlg::default()
                },
            )
        }
        Alg::Ed25519 | Alg::X25519 => {
            if priv_usages == 0 {
                return Err(empty_usages());
            }
            let id = if alg == Alg::Ed25519 {
                Id::ED25519
            } else {
                Id::X25519
            };
            (
                PKey::generate(id).map_err(|_| unexpected())?,
                KeyAlg {
                    id: alg,
                    ..KeyAlg::default()
                },
            )
        }
        _ => return Err(unexpected()),
    };
    let pk = public_of(&sk)?;
    let public = CryptoKey {
        ty: KeyType::Public,
        extractable: true,
        alg: ka.clone(),
        usages: pub_usages,
        mat: Material::Public(pk),
    };
    let private = CryptoKey {
        ty: KeyType::Private,
        extractable,
        alg: ka,
        usages: priv_usages,
        mat: Material::Private(sk),
    };
    Ok(Generated::Pair(private, public))
}

/// The value of a big-endian `publicExponent` (leading zeros allowed).
fn exponent_value(b: &[u8]) -> Option<u64> {
    let b: Vec<u8> = b.iter().copied().skip_while(|x| *x == 0).collect();
    if b.len() > 8 {
        return None;
    }
    Some(b.iter().fold(0u64, |a, x| (a << 8) | u64::from(*x)))
}

/// The public half of a private key (through its SPKI encoding).
fn public_of(sk: &PKey<Private>) -> Result<PKey<Public>, Fail> {
    let der = sk.public_key_to_der().map_err(|_| unexpected())?;
    PKey::public_key_from_der(&der).map_err(|_| unexpected())
}

// ---------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------

fn unsupported_import() -> Fail {
    Fail::unsupported("Unsupported import key format for algorithm")
}

fn unsupported_export() -> Fail {
    Fail::unsupported("Unsupported export key format for algorithm")
}

pub(crate) fn import(
    format: Format,
    data: &KeyData,
    p: &Params,
    extractable: bool,
    usages: u8,
) -> Result<CryptoKey, Fail> {
    let alg = p.alg;
    match alg {
        a if is_aes(a) || a == Alg::Hmac || a == Alg::Hkdf || a == Alg::Pbkdf2 => {
            // VerifyKeyUsagesBeforeImportKey: format first, then usages.
            let formats_ok = match a {
                Alg::Hkdf | Alg::Pbkdf2 => format == Format::Raw,
                _ => matches!(format, Format::Raw | Format::Jwk),
            };
            if !formats_ok {
                return Err(unsupported_import());
            }
            if !contains(secret_usages(a), usages) {
                return Err(bad_usages());
            }
            if matches!(a, Alg::Hkdf | Alg::Pbkdf2) && extractable {
                return Err(Fail::syntax("KDF keys must set extractable=false"));
            }
            let raw = match data {
                KeyData::Bytes(b) => b.clone(),
                KeyData::Jwk(j) => {
                    let expected_alg = if a == Alg::Hmac {
                        hmac_jwk_alg(p.hash.unwrap_or(Alg::Sha256))
                    } else {
                        String::new()
                    };
                    let raw = j.read_secret(extractable, usages, &expected_alg)?;
                    if is_aes(a) {
                        jwk::check_aes_alg(j, a, raw.len())?;
                    }
                    raw
                }
            };
            import_secret(p, extractable, usages, raw)
        }
        _ => import_asym(format, data, p, extractable, usages),
    }
}

fn hmac_jwk_alg(h: Alg) -> String {
    match h {
        Alg::Sha1 => "HS1",
        Alg::Sha384 => "HS384",
        Alg::Sha512 => "HS512",
        _ => "HS256",
    }
    .into()
}

pub(crate) fn aes_jwk_suffix(a: Alg) -> &'static str {
    match a {
        Alg::AesGcm => "GCM",
        Alg::AesCbc => "CBC",
        Alg::AesCtr => "CTR",
        _ => "KW",
    }
}

fn import_secret(
    p: &Params,
    extractable: bool,
    usages: u8,
    raw: Vec<u8>,
) -> Result<CryptoKey, Fail> {
    let a = p.alg;
    let bits = (raw.len() * 8) as u32;
    let ka = match a {
        a if is_aes(a) => {
            match raw.len() {
                16 | 32 => {}
                24 => return Err(Fail::op("192-bit AES keys are not supported")),
                _ => return Err(Fail::data("AES key data must be 128 or 256 bits")),
            }
            KeyAlg {
                id: a,
                length: bits,
                ..KeyAlg::default()
            }
        }
        Alg::Hmac => {
            if raw.is_empty() {
                return Err(Fail::data("HMAC key data must not be empty"));
            }
            let length = match p.length {
                Some(l) if l > bits || l + 8 <= bits || l == 0 => {
                    return Err(Fail::data(
                        "The optional HMAC key length must be shorter than the key data, and by no more than 7 bits.",
                    ));
                }
                Some(l) => l,
                None => bits,
            };
            KeyAlg {
                id: a,
                hash: p.hash,
                length,
                ..KeyAlg::default()
            }
        }
        _ => KeyAlg {
            id: a,
            ..KeyAlg::default()
        },
    };
    if usages == 0 {
        return Err(empty_usages());
    }
    let raw = if a == Alg::Hmac {
        truncate_bits(raw, ka.length)
    } else {
        raw
    };
    Ok(secret_key(ka, extractable, usages, raw))
}

fn import_asym(
    format: Format,
    data: &KeyData,
    p: &Params,
    extractable: bool,
    usages: u8,
) -> Result<CryptoKey, Fail> {
    let alg = p.alg;
    let (pub_all, priv_all) = asym_usages(alg);
    // VerifyUsagesBeforeImportAsymmetricKey.
    let allowed = match format {
        Format::Spki => pub_all,
        Format::Pkcs8 => priv_all,
        Format::Jwk => pub_all | priv_all,
        Format::Raw => {
            if is_rsa(alg) {
                return Err(unsupported_import());
            }
            pub_all
        }
    };
    if !contains(allowed, usages) {
        return Err(bad_usages());
    }
    let want_id = if is_rsa(alg) {
        Id::RSA
    } else if alg == Alg::Ed25519 {
        Id::ED25519
    } else if alg == Alg::X25519 {
        Id::X25519
    } else {
        Id::EC
    };
    let mat = match (format, data) {
        (Format::Spki, KeyData::Bytes(b)) => {
            let pk = PKey::public_key_from_der(b).map_err(|_| Fail::data(""))?;
            if pk.id() != want_id {
                return Err(Fail::data(""));
            }
            Material::Public(pk)
        }
        (Format::Pkcs8, KeyData::Bytes(b)) => {
            let sk = PKey::private_key_from_pkcs8(b).map_err(|_| Fail::data(""))?;
            if sk.id() != want_id {
                return Err(Fail::data(""));
            }
            Material::Private(sk)
        }
        (Format::Raw, KeyData::Bytes(b)) => match alg {
            Alg::Ecdsa | Alg::Ecdh => {
                let c = p.curve.unwrap_or(Curve::P256);
                let g = EcGroup::from_curve_name(curve_nid(c)).map_err(|_| unexpected())?;
                let mut ctx = BigNumContext::new().map_err(|_| unexpected())?;
                let pt = EcPoint::from_bytes(&g, b, &mut ctx).map_err(|_| Fail::data(""))?;
                let ec = EcKey::from_public_key(&g, &pt).map_err(|_| Fail::data(""))?;
                Material::Public(PKey::from_ec_key(ec).map_err(|_| unexpected())?)
            }
            _ => {
                let pk = raw_okp_public(want_id, b)?;
                Material::Public(pk)
            }
        },
        (Format::Jwk, KeyData::Jwk(j)) => jwk::import_asym(j, p, extractable, usages)?,
        _ => return Err(unexpected()),
    };
    let (ty, mut ka) = match &mat {
        Material::Private(sk) => (KeyType::Private, alg_of_pkey_private(sk, p)?),
        Material::Public(pk) => (KeyType::Public, alg_of_pkey_public(pk, p)?),
        Material::Secret(_) => return Err(unexpected()),
    };
    ka.hash = p.hash;
    let usages = match ty {
        KeyType::Private => {
            if !contains(priv_all, usages) {
                return Err(bad_usages());
            }
            if usages == 0 {
                return Err(empty_usages());
            }
            usages
        }
        _ => {
            if !contains(pub_all, usages) {
                return Err(bad_usages());
            }
            usages
        }
    };
    Ok(CryptoKey {
        ty,
        extractable,
        alg: ka,
        usages,
        mat,
    })
}

/// Public key from raw bytes (Ed25519 / X25519): `EVP_PKEY_new_raw_public_key`,
/// which `boring` does not wrap.
fn raw_okp_public(id: Id, b: &[u8]) -> Result<PKey<Public>, Fail> {
    if b.len() != 32 {
        return Err(Fail::data(""));
    }
    // SAFETY: the pointer and length describe `b`, which outlives the
    // call; BoringSSL copies the bytes. A non-null result is a new
    // EVP_PKEY we own, handed to `PKey` (which frees it).
    unsafe {
        let p = boring_sys::EVP_PKEY_new_raw_public_key(
            id.as_raw(),
            std::ptr::null_mut(),
            b.as_ptr(),
            b.len(),
        );
        if p.is_null() {
            return Err(Fail::data(""));
        }
        Ok(<PKey<Public> as foreign_types::ForeignType>::from_ptr(p))
    }
}

/// Private key from its raw 32-byte seed (Ed25519 / X25519 JWK `d`).
pub(crate) fn raw_okp_private(id: Id, b: &[u8]) -> Result<PKey<Private>, Fail> {
    if b.len() != 32 {
        return Err(Fail::data(""));
    }
    // SAFETY: as `raw_okp_public`.
    unsafe {
        let p = boring_sys::EVP_PKEY_new_raw_private_key(
            id.as_raw(),
            std::ptr::null_mut(),
            b.as_ptr(),
            b.len(),
        );
        if p.is_null() {
            return Err(Fail::data(""));
        }
        Ok(<PKey<Private> as foreign_types::ForeignType>::from_ptr(p))
    }
}

pub(crate) fn raw_okp_from_public(id: Id, b: &[u8]) -> Result<PKey<Public>, Fail> {
    raw_okp_public(id, b)
}

fn ec_curve_of(ec_group_nid: Option<Nid>) -> Option<Curve> {
    match ec_group_nid? {
        Nid::X9_62_PRIME256V1 => Some(Curve::P256),
        Nid::SECP384R1 => Some(Curve::P384),
        Nid::SECP521R1 => Some(Curve::P521),
        _ => None,
    }
}

fn check_curve(found: Option<Curve>, p: &Params) -> Result<Curve, Fail> {
    let want = p.curve.unwrap_or(Curve::P256);
    match found {
        Some(c) if c == want => Ok(c),
        _ => Err(Fail::data(
            "The imported EC key specifies a different curve than requested",
        )),
    }
}

fn alg_of_pkey_private(sk: &PKey<Private>, p: &Params) -> Result<KeyAlg, Fail> {
    match p.alg {
        Alg::Ecdsa | Alg::Ecdh => {
            let ec = sk.ec_key().map_err(|_| Fail::data(""))?;
            let c = check_curve(ec_curve_of(ec.group().curve_name()), p)?;
            if ec.check_key().is_err() {
                return Err(Fail::data("The imported EC key is invalid"));
            }
            Ok(KeyAlg {
                id: p.alg,
                curve: Some(c),
                ..KeyAlg::default()
            })
        }
        a if is_rsa(a) => {
            let rsa = sk.rsa().map_err(|_| Fail::data(""))?;
            Ok(KeyAlg {
                id: a,
                modulus: rsa.n().num_bits() as u32,
                exponent: rsa.e().to_vec(),
                ..KeyAlg::default()
            })
        }
        a => Ok(KeyAlg {
            id: a,
            ..KeyAlg::default()
        }),
    }
}

fn alg_of_pkey_public(pk: &PKey<Public>, p: &Params) -> Result<KeyAlg, Fail> {
    match p.alg {
        Alg::Ecdsa | Alg::Ecdh => {
            let ec = pk.ec_key().map_err(|_| Fail::data(""))?;
            let c = check_curve(ec_curve_of(ec.group().curve_name()), p)?;
            if ec.check_key().is_err() {
                return Err(Fail::data("The imported EC key is invalid"));
            }
            Ok(KeyAlg {
                id: p.alg,
                curve: Some(c),
                ..KeyAlg::default()
            })
        }
        a if is_rsa(a) => {
            let rsa = pk.rsa().map_err(|_| Fail::data(""))?;
            Ok(KeyAlg {
                id: a,
                modulus: rsa.n().num_bits() as u32,
                exponent: rsa.e().to_vec(),
                ..KeyAlg::default()
            })
        }
        a => Ok(KeyAlg {
            id: a,
            ..KeyAlg::default()
        }),
    }
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

fn wrong_type() -> Fail {
    Fail::access("The key is not of the expected type")
}

pub(crate) fn export(format: Format, key: &CryptoKey) -> Result<Exported, Fail> {
    let alg = key.alg.id;
    match &key.mat {
        Material::Secret(b) => match format {
            Format::Raw if !matches!(alg, Alg::Hkdf | Alg::Pbkdf2) => {
                Ok(Exported::Bytes(b.clone()))
            }
            Format::Jwk if !matches!(alg, Alg::Hkdf | Alg::Pbkdf2) => {
                let a = if alg == Alg::Hmac {
                    hmac_jwk_alg(key.alg.hash.unwrap_or(Alg::Sha256))
                } else {
                    jwk::aes_alg_name(alg, b.len())
                };
                let mut o = JwkOut::new("oct", key);
                o.str("alg", &a);
                o.str("k", &jwk::b64url(b));
                Ok(Exported::Jwk(o.finish()))
            }
            _ => Err(unsupported_export()),
        },
        Material::Public(pk) => match format {
            Format::Spki => Ok(Exported::Bytes(
                pk.public_key_to_der().map_err(|_| unexpected())?,
            )),
            Format::Raw if !is_rsa(alg) => Ok(Exported::Bytes(public_raw(pk, alg)?)),
            Format::Jwk => Ok(Exported::Jwk(jwk::export_public(pk, key)?)),
            Format::Pkcs8 => Err(wrong_type()),
            _ => Err(unsupported_export()),
        },
        Material::Private(sk) => match format {
            Format::Pkcs8 => Ok(Exported::Bytes(
                sk.private_key_to_der_pkcs8().map_err(|_| unexpected())?,
            )),
            Format::Jwk => Ok(Exported::Jwk(jwk::export_private(sk, key)?)),
            Format::Raw if is_rsa(alg) => Err(unsupported_export()),
            _ => Err(wrong_type()),
        },
    }
}

/// Uncompressed EC point, or the raw OKP public key.
pub(crate) fn public_raw(pk: &PKey<Public>, alg: Alg) -> Result<Vec<u8>, Fail> {
    if matches!(alg, Alg::Ecdsa | Alg::Ecdh) {
        let ec = pk.ec_key().map_err(|_| unexpected())?;
        let mut ctx = BigNumContext::new().map_err(|_| unexpected())?;
        return ec
            .public_key()
            .to_bytes(ec.group(), PointConversionForm::UNCOMPRESSED, &mut ctx)
            .map_err(|_| unexpected());
    }
    let mut out = [0u8; 32];
    let n = pk.raw_public_key(&mut out).map_err(|_| unexpected())?.len();
    Ok(out[..n].to_vec())
}

pub(crate) fn rsa_jwk_alg(alg: Alg, hash: Alg) -> &'static str {
    match (alg, hash) {
        (Alg::RsaPkcs1, Alg::Sha1) => "RS1",
        (Alg::RsaPkcs1, Alg::Sha256) => "RS256",
        (Alg::RsaPkcs1, Alg::Sha384) => "RS384",
        (Alg::RsaPkcs1, Alg::Sha512) => "RS512",
        (Alg::RsaPss, Alg::Sha1) => "PS1",
        (Alg::RsaPss, Alg::Sha256) => "PS256",
        (Alg::RsaPss, Alg::Sha384) => "PS384",
        (Alg::RsaPss, Alg::Sha512) => "PS512",
        (Alg::RsaOaep, Alg::Sha1) => "RSA-OAEP",
        (Alg::RsaOaep, Alg::Sha256) => "RSA-OAEP-256",
        (Alg::RsaOaep, Alg::Sha384) => "RSA-OAEP-384",
        _ => "RSA-OAEP-512",
    }
}

pub(crate) fn nid_for_curve(c: Curve) -> Nid {
    curve_nid(c)
}

pub(crate) fn curve_bytes(c: Curve) -> usize {
    field_bytes(c)
}

pub(crate) fn asym_usage_sets(alg: Alg) -> (u8, u8) {
    asym_usages(alg)
}

// ---------------------------------------------------------------------------
// RSA-OAEP (BoringSSL EVP_PKEY_encrypt with OAEP digests and label)
// ---------------------------------------------------------------------------

fn rsa_oaep(p: &Params, key: &CryptoKey, data: &[u8], enc: bool) -> Result<Vec<u8>, Fail> {
    use boring_sys as ffi;
    use foreign_types::ForeignTypeRef;
    let h = md(key.alg.hash.unwrap_or(Alg::Sha1));
    let label = p.label.as_deref().unwrap_or(&[]);
    let pkey: *mut ffi::EVP_PKEY = match (&key.mat, enc) {
        (Material::Public(pk), true) => pk.as_ref().as_ptr(),
        (Material::Private(sk), false) => sk.as_ref().as_ptr(),
        _ => return Err(wrong_type()),
    };
    // SAFETY: plain BoringSSL calls on a key we hold for the duration;
    // the context is freed on every path; the label is copied into memory
    // from OPENSSL_malloc, whose ownership passes to the context (set0);
    // output buffers are sized from the length query first.
    unsafe {
        let ctx = ffi::EVP_PKEY_CTX_new(pkey, std::ptr::null_mut());
        if ctx.is_null() {
            return Err(unexpected());
        }
        let r = (|| {
            let init = if enc {
                ffi::EVP_PKEY_encrypt_init(ctx)
            } else {
                ffi::EVP_PKEY_decrypt_init(ctx)
            };
            if init != 1
                || ffi::EVP_PKEY_CTX_set_rsa_padding(ctx, ffi::RSA_PKCS1_OAEP_PADDING) != 1
                || ffi::EVP_PKEY_CTX_set_rsa_oaep_md(ctx, h.as_ptr()) != 1
                || ffi::EVP_PKEY_CTX_set_rsa_mgf1_md(ctx, h.as_ptr()) != 1
            {
                return None;
            }
            if !label.is_empty() {
                let buf = ffi::OPENSSL_malloc(label.len()) as *mut u8;
                if buf.is_null() {
                    return None;
                }
                std::ptr::copy_nonoverlapping(label.as_ptr(), buf, label.len());
                if ffi::EVP_PKEY_CTX_set0_rsa_oaep_label(ctx, buf, label.len()) != 1 {
                    ffi::OPENSSL_free(buf as *mut _);
                    return None;
                }
            }
            let mut len: usize = 0;
            let ok = if enc {
                ffi::EVP_PKEY_encrypt(
                    ctx,
                    std::ptr::null_mut(),
                    &mut len,
                    data.as_ptr(),
                    data.len(),
                )
            } else {
                ffi::EVP_PKEY_decrypt(
                    ctx,
                    std::ptr::null_mut(),
                    &mut len,
                    data.as_ptr(),
                    data.len(),
                )
            };
            if ok != 1 {
                return None;
            }
            let mut out = vec![0u8; len];
            let ok = if enc {
                ffi::EVP_PKEY_encrypt(ctx, out.as_mut_ptr(), &mut len, data.as_ptr(), data.len())
            } else {
                ffi::EVP_PKEY_decrypt(ctx, out.as_mut_ptr(), &mut len, data.as_ptr(), data.len())
            };
            if ok != 1 {
                return None;
            }
            out.truncate(len);
            Some(out)
        })();
        ffi::EVP_PKEY_CTX_free(ctx);
        ffi::ERR_clear_error();
        r.ok_or_else(unexpected)
    }
}

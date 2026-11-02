//! JA3 and JA4 (FoxIO spec) from a parsed ClientHello.

use md5::{Digest as _, Md5};
use sha2::Sha256;

use crate::clienthello::{ClientHello, ext, is_grease};

fn join_dec<T: ToString>(v: impl IntoIterator<Item = T>) -> String {
    v.into_iter()
        .map(|x| x.to_string())
        .collect::<Vec<_>>()
        .join("-")
}

/// JA3 string: `version,ciphers,extensions,groups,point_formats`, decimal,
/// GREASE removed.
pub fn ja3_string(ch: &ClientHello) -> String {
    let nog = |v: &u16| !is_grease(*v);
    format!(
        "{},{},{},{},{}",
        ch.version,
        join_dec(ch.cipher_suites.iter().filter(|v| nog(v))),
        join_dec(ch.extensions.iter().map(|e| e.id).filter(nog)),
        join_dec(ch.supported_groups.iter().filter(|v| nog(v))),
        join_dec(ch.ec_point_formats.iter()),
    )
}

/// MD5 of a JA3 string, lowercase hex.
pub fn ja3_hash(ja3_string: &str) -> String {
    hex::encode(Md5::digest(ja3_string.as_bytes()))
}

pub fn ja3(ch: &ClientHello) -> String {
    ja3_hash(&ja3_string(ch))
}

/// First 12 hex chars of SHA-256, or `000000000000` for empty input.
pub fn sha256_12(s: &str) -> String {
    if s.is_empty() {
        return "000000000000".into();
    }
    hex::encode(Sha256::digest(s.as_bytes()))[..12].to_owned()
}

fn version_code(v: u16) -> &'static str {
    match v {
        0x0304 => "13",
        0x0303 => "12",
        0x0302 => "11",
        0x0301 => "10",
        0x0300 => "s3",
        0x0002 => "s2",
        0xfeff => "d1",
        0xfefd => "d2",
        0xfefc => "d3",
        _ => "00",
    }
}

fn alpn_code(ch: &ClientHello) -> String {
    let Some(first) = ch.alpn.first().map(|s| s.as_bytes()) else {
        return "00".into();
    };
    let (Some(&a), Some(&b)) = (first.first(), first.last()) else {
        return "00".into();
    };
    if a.is_ascii_alphanumeric() && b.is_ascii_alphanumeric() {
        format!("{}{}", a as char, b as char)
    } else {
        // Spec: non-alphanumeric -> first hex digit of the first byte and
        // last hex digit of the last byte.
        let h = format!("{a:02x}{b:02x}");
        format!("{}{}", &h[0..1], &h[3..4])
    }
}

/// The parts of a JA4, before and after hashing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ja4 {
    /// `t13d1516h2`.
    pub a: String,
    /// Sorted cipher hex list, comma joined (JA4_r part b).
    pub ciphers_raw: String,
    /// Sorted extensions without SNI/ALPN, `_`, sigalgs in wire order.
    pub extensions_raw: String,
}

impl Ja4 {
    pub fn from_client_hello(ch: &ClientHello, transport: char) -> Self {
        let ciphers: Vec<u16> = ch
            .cipher_suites
            .iter()
            .copied()
            .filter(|&v| !is_grease(v))
            .collect();
        let exts: Vec<u16> = ch
            .extension_ids()
            .into_iter()
            .filter(|&v| !is_grease(v))
            .collect();
        let sni = if ch.has_extension(ext::SERVER_NAME) {
            'd'
        } else {
            'i'
        };
        let a = format!(
            "{transport}{}{sni}{:02}{:02}{}",
            version_code(ch.max_version()),
            ciphers.len().min(99),
            exts.len().min(99),
            alpn_code(ch)
        );
        let mut c: Vec<String> = ciphers.iter().map(|v| format!("{v:04x}")).collect();
        c.sort();
        let mut e: Vec<String> = exts
            .iter()
            .filter(|&&v| v != ext::SERVER_NAME && v != ext::ALPN)
            .map(|v| format!("{v:04x}"))
            .collect();
        e.sort();
        let sig: Vec<String> = ch
            .signature_algorithms
            .iter()
            .filter(|&&v| !is_grease(v))
            .map(|v| format!("{v:04x}"))
            .collect();
        let mut extensions_raw = e.join(",");
        if !sig.is_empty() {
            extensions_raw.push('_');
            extensions_raw.push_str(&sig.join(","));
        }
        Self {
            a,
            ciphers_raw: c.join(","),
            extensions_raw,
        }
    }

    /// Hashed JA4, e.g. `t13d1516h2_8daaf6152771_b186095e22b6`.
    pub fn hashed(&self) -> String {
        let e = if self.extensions_raw.is_empty() || self.extensions_raw.starts_with('_') {
            "000000000000".to_owned()
        } else {
            sha256_12(&self.extensions_raw)
        };
        format!("{}_{}_{}", self.a, sha256_12(&self.ciphers_raw), e)
    }

    /// JA4_r: the unhashed form.
    pub fn raw(&self) -> String {
        format!("{}_{}_{}", self.a, self.ciphers_raw, self.extensions_raw)
    }
}

/// JA4 over TCP (`t...`), hashed.
pub fn ja4(ch: &ClientHello) -> String {
    Ja4::from_client_hello(ch, 't').hashed()
}

/// Akamai HTTP/2 fingerprint text:
/// `id:value;...|window_update|priority_frames|pseudo_header_order`.
pub fn akamai_h2(
    settings: &[(u16, u32)],
    window_update: u32,
    priority_frames: &[String],
    pseudo_headers: &[String],
) -> String {
    let s = settings
        .iter()
        .map(|(k, v)| format!("{k}:{v}"))
        .collect::<Vec<_>>()
        .join(";");
    let p = if priority_frames.is_empty() {
        "0".to_owned()
    } else {
        priority_frames.join(",")
    };
    let h = pseudo_headers
        .iter()
        .map(|n| n.trim_start_matches(':').chars().next().unwrap_or('?'))
        .map(String::from)
        .collect::<Vec<_>>()
        .join(",");
    format!("{s}|{window_update}|{p}|{h}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clienthello::{Extension, build};
    use serde_json::Value;

    const FIREFOX: &str = include_str!("../../../profiles/tls/firefox_128_win10.json");

    fn ids(v: &Value, key: &str) -> Vec<u16> {
        v[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["id"].as_u64().unwrap() as u16)
            .collect()
    }

    /// Rebuilds the Firefox 128 ClientHello from the profile's lists, using
    /// the extension order of the recorded `ja3_string`.
    fn firefox_hello(p: &Value) -> ClientHello {
        let ja3s = p["ja3_string"].as_str().unwrap();
        let ext_order: Vec<u16> = ja3s
            .split(',')
            .nth(2)
            .unwrap()
            .split('-')
            .map(|s| s.parse().unwrap())
            .collect();
        let groups = ids(p, "supported_groups");
        let sigalgs = ids(p, "signature_algorithms");
        let alpn: Vec<&str> = p["alpn_protocols"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap())
            .collect();
        let extensions = ext_order
            .into_iter()
            .map(|id| {
                let data = match id {
                    ext::SERVER_NAME => build::sni("localhost"),
                    ext::SUPPORTED_GROUPS => build::u16_vec16(&groups),
                    ext::EC_POINT_FORMATS => vec![1, 0],
                    ext::SIGNATURE_ALGORITHMS => build::u16_vec16(&sigalgs),
                    ext::ALPN => build::protocols(&alpn),
                    ext::SUPPORTED_VERSIONS => build::u16_vec8(&[0x0304, 0x0303]),
                    ext::KEY_SHARE => build::key_share(&[29, 23]),
                    ext::DELEGATED_CREDENTIALS => {
                        build::u16_vec16(&[0x0403, 0x0503, 0x0603, 0x0203])
                    }
                    ext::RECORD_SIZE_LIMIT => 0x4001u16.to_be_bytes().to_vec(),
                    ext::PSK_KEY_EXCHANGE_MODES => vec![1, 1],
                    ext::RENEGOTIATION_INFO => vec![0],
                    _ => vec![],
                };
                Extension { id, data }
            })
            .collect();
        ClientHello {
            record_version: 0x0301,
            version: 0x0303,
            random: [0; 32],
            session_id: vec![0; 32],
            cipher_suites: ids(p, "cipher_suites"),
            compression_methods: vec![0],
            extensions,
            ..Default::default()
        }
    }

    #[test]
    fn firefox_ja3_matches_recorded() {
        let p: Value = serde_json::from_str(FIREFOX).unwrap();
        let rec = firefox_hello(&p).to_record();
        let ch = ClientHello::parse_records(&rec).unwrap();
        assert_eq!(ja3_string(&ch), p["ja3_string"].as_str().unwrap());
        // The profile's recorded `ja3` (579ccef3...) is NOT the MD5 of its
        // own `ja3_string`; all three firefox_128_* files share one string
        // but list different hashes. The MD5 below was checked with Python's
        // hashlib.
        assert_eq!(ja3(&ch), "b5001237acdf006056b409cc433726b0");
    }

    #[test]
    fn ja3_reference_example() {
        // Example from the salesforce/ja3 README.
        assert_eq!(
            ja3_hash("769,47-53-5-10-49161-49162-49171-49172-50-56-19-4,0-10-11,23-24-25,0"),
            "ada70206e40642a3e4461f35503241d5"
        );
    }

    #[test]
    fn firefox_ja4_prefix_and_ciphers_match_recorded() {
        // The recorded `ja4` is a raw (JA4_r style) string. Its prefix and
        // cipher part agree with the profile; its extension and sigalg parts
        // were taken from a different capture (they list 0x001b/0x0010 and
        // drop 0x0203), so only the first two parts are compared.
        let p: Value = serde_json::from_str(FIREFOX).unwrap();
        let ch = ClientHello::parse_records(&firefox_hello(&p).to_record()).unwrap();
        let j = Ja4::from_client_hello(&ch, 't');
        let recorded: Vec<&str> = p["ja4"].as_str().unwrap().split('_').collect();
        assert_eq!(j.a, recorded[0]);
        assert_eq!(j.ciphers_raw, recorded[1]);
        // Firefox's well-known JA4 cipher hash.
        assert_eq!(sha256_12(&j.ciphers_raw), "5b57614c22b0");
    }

    #[test]
    fn foxio_spec_example() {
        // Example from the FoxIO JA4 technical details (Chrome).
        let ciphers = "002f,0035,009c,009d,1301,1302,1303,c013,c014,c02b,c02c,c02f,c030,cca8,cca9";
        let exts = "0005,000a,000b,000d,0012,0015,0017,001b,0023,002b,002d,0033,4469,ff01_0403,0804,0401,0503,0805,0501,0806,0601";
        assert_eq!(sha256_12(ciphers), "8daaf6152771");
        assert_eq!(sha256_12(exts), "e5627efa2ab1");
    }

    #[test]
    fn ja4_from_hello_ignores_grease_order_and_sni_alpn() {
        let p: Value = serde_json::from_str(FIREFOX).unwrap();
        let mut ch = firefox_hello(&p);
        let a = Ja4::from_client_hello(&ch, 't');
        ch.extensions.reverse();
        ch.extensions.insert(
            0,
            Extension {
                id: 0x0a0a,
                data: vec![],
            },
        );
        ch.cipher_suites.insert(0, 0x2a2a);
        let b = Ja4::from_client_hello(&ch, 't');
        assert_eq!(a, b);
        assert!(!a.extensions_raw.contains("0000,") && !a.extensions_raw.contains("0010"));
        assert_eq!(a.hashed().len(), "t13d1715h2_".len() + 12 + 1 + 12);
    }

    #[test]
    fn alpn_codes() {
        let mut ch = ClientHello::default();
        assert_eq!(alpn_code(&ch), "00");
        ch.alpn = vec!["http/1.1".into()];
        assert_eq!(alpn_code(&ch), "h1");
        ch.alpn = vec!["h2".into()];
        assert_eq!(alpn_code(&ch), "h2");
    }

    #[test]
    fn akamai_text() {
        let s = akamai_h2(
            &[(1, 65536), (2, 0), (4, 6291456), (6, 262144)],
            15663105,
            &[],
            &[
                ":method".into(),
                ":authority".into(),
                ":scheme".into(),
                ":path".into(),
            ],
        );
        assert_eq!(s, "1:65536;2:0;4:6291456;6:262144|15663105|0|m,a,s,p");
    }
}

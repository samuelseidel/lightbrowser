//! Builds a `profiles/tls/*.json` TLS/H2 profile from captures, in the same
//! shape as `firefox_128_win10.json` plus the Chrome knobs (`grease`,
//! `permute_extensions`, `alps_protocols`, `ech_grease`).

use std::collections::BTreeSet;
use std::fmt::Write as _;

use crate::clienthello::{ClientHello, Extension, ext, is_grease};
use crate::fingerprint::{Ja4, ja3_hash, ja3_string};
use crate::h2::setting_name;
use crate::server::{Capture, H2Capture};

/// Chrome's un-permuted extension order (BoringSSL table order), without
/// GREASE, which BoringSSL always puts first and last. ALPS is listed by its
/// old codepoint; the observed codepoint replaces it.
pub const CHROME_CANONICAL_ORDER: [u16; 18] = [
    0, 23, 65281, 10, 11, 35, 16, 5, 13, 18, 51, 45, 43, 27, 17513, 51764, 4832, 65037,
];

pub fn cipher_name(id: u16) -> String {
    let n = match id {
        0x1301 => "TLS_AES_128_GCM_SHA256",
        0x1302 => "TLS_AES_256_GCM_SHA384",
        0x1303 => "TLS_CHACHA20_POLY1305_SHA256",
        0xc02b => "TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256",
        0xc02f => "TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256",
        0xc02c => "TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384",
        0xc030 => "TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384",
        0xcca9 => "TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256",
        0xcca8 => "TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256",
        0xc009 => "TLS_ECDHE_ECDSA_WITH_AES_128_CBC_SHA",
        0xc00a => "TLS_ECDHE_ECDSA_WITH_AES_256_CBC_SHA",
        0xc013 => "TLS_ECDHE_RSA_WITH_AES_128_CBC_SHA",
        0xc014 => "TLS_ECDHE_RSA_WITH_AES_256_CBC_SHA",
        0x009c => "TLS_RSA_WITH_AES_128_GCM_SHA256",
        0x009d => "TLS_RSA_WITH_AES_256_GCM_SHA384",
        0x002f => "TLS_RSA_WITH_AES_128_CBC_SHA",
        0x0035 => "TLS_RSA_WITH_AES_256_CBC_SHA",
        0x000a => "TLS_RSA_WITH_3DES_EDE_CBC_SHA",
        _ => return format!("unknown_0x{id:04X}"),
    };
    n.into()
}

pub fn extension_name(id: u16) -> String {
    let n = match id {
        0 => "server_name",
        5 => "status_request",
        10 => "supported_groups",
        11 => "ec_point_formats",
        13 => "signature_algorithms",
        16 => "alpn",
        18 => "signed_certificate_timestamp",
        21 => "padding",
        23 => "extended_master_secret",
        27 => "compress_certificate",
        28 => "record_size_limit",
        34 => "delegated_credentials",
        35 => "session_ticket",
        41 => "pre_shared_key",
        42 => "early_data",
        43 => "supported_versions",
        44 => "cookie",
        45 => "psk_key_exchange_modes",
        50 => "signature_algorithms_cert",
        51 => "key_share",
        17513 => "application_settings_old",
        17613 => "application_settings",
        4832 => "server_padding",
        51764 => "trust_anchors",
        65037 => "encrypted_client_hello",
        65281 => "renegotiation_info",
        _ if is_grease(id) => "grease",
        _ => return format!("unknown_0x{id:04X}"),
    };
    n.into()
}

pub fn group_name(id: u16) -> String {
    let n = match id {
        29 => "x25519",
        30 => "x448",
        23 => "secp256r1",
        24 => "secp384r1",
        25 => "secp521r1",
        256 => "ffdhe2048",
        257 => "ffdhe3072",
        4588 => "X25519MLKEM768",
        25497 => "X25519Kyber768Draft00",
        _ => return format!("unknown_0x{id:04X}"),
    };
    n.into()
}

pub fn sigalg_name(id: u16) -> String {
    let n = match id {
        0x0403 => "ecdsa_secp256r1_sha256",
        0x0503 => "ecdsa_secp384r1_sha384",
        0x0603 => "ecdsa_secp521r1_sha512",
        0x0804 => "rsa_pss_rsae_sha256",
        0x0805 => "rsa_pss_rsae_sha384",
        0x0806 => "rsa_pss_rsae_sha512",
        0x0807 => "ed25519",
        0x0808 => "ed448",
        0x0401 => "rsa_pkcs1_sha256",
        0x0501 => "rsa_pkcs1_sha384",
        0x0601 => "rsa_pkcs1_sha512",
        0x0203 => "ecdsa_sha1",
        0x0201 => "rsa_pkcs1_sha1",
        0x0904 => "mldsa44",
        0x0905 => "mldsa65",
        0x0906 => "mldsa87",
        _ => return format!("unknown_0x{id:04X}"),
    };
    n.into()
}

fn point_format_name(id: u8) -> &'static str {
    match id {
        0 => "uncompressed",
        1 => "ansiX962_compressed_prime",
        2 => "ansiX962_compressed_char2",
        _ => "unknown",
    }
}

fn cert_compression_name(id: u16) -> String {
    match id {
        1 => "zlib".into(),
        2 => "brotli".into(),
        3 => "zstd".into(),
        _ => format!("unknown_{id}"),
    }
}

fn tls_version_name(v: u16) -> String {
    match v {
        0x0304 => "TLS 1.3".into(),
        0x0303 => "TLS 1.2".into(),
        0x0302 => "TLS 1.1".into(),
        0x0301 => "TLS 1.0".into(),
        _ => format!("0x{v:04X}"),
    }
}

/// Non-GREASE extension ids in wire order.
pub fn extension_order(ch: &ClientHello) -> Vec<u16> {
    ch.extension_ids()
        .into_iter()
        .filter(|&id| !is_grease(id))
        .collect()
}

/// True for a resumption hello (pre_shared_key / early_data present). Those
/// add extensions, so they are left out of profile building.
pub fn is_resumption(ch: &ClientHello) -> bool {
    ch.has_extension(ext::PRE_SHARED_KEY) || ch.has_extension(42)
}

/// Result of comparing the extension order of several captures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderCheck {
    /// Orders differ between captures.
    pub permuted: bool,
    /// Every capture has the same extension set.
    pub same_set: bool,
}

pub fn compare_orders(hellos: &[&ClientHello]) -> OrderCheck {
    let orders: Vec<Vec<u16>> = hellos.iter().map(|c| extension_order(c)).collect();
    let sets: Vec<BTreeSet<u16>> = orders.iter().map(|o| o.iter().copied().collect()).collect();
    OrderCheck {
        permuted: orders.windows(2).any(|w| w[0] != w[1]),
        same_set: sets.windows(2).all(|w| w[0] == w[1]),
    }
}

/// Chrome's canonical order restricted to `observed`, with the observed ALPS
/// codepoint, and unknown extensions appended in observed order.
pub fn canonical_chrome_order(observed: &[u16], notes: &mut Vec<String>) -> Vec<u16> {
    let alps = if observed.contains(&ext::ALPS_NEW) {
        ext::ALPS_NEW
    } else {
        ext::ALPS_OLD
    };
    let mut out: Vec<u16> = CHROME_CANONICAL_ORDER
        .iter()
        .map(|&id| if id == ext::ALPS_OLD { alps } else { id })
        .filter(|id| observed.contains(id))
        .collect();
    for &id in observed {
        if !out.contains(&id) {
            notes.push(format!(
                "extension {id} ({}) is not in the known Chrome order; appended at the end",
                extension_name(id)
            ));
            out.push(id);
        }
    }
    out
}

/// Identity fields that do not come from the wire.
#[derive(Debug, Clone)]
pub struct ProfileMeta {
    pub profile_id: String,
    pub description: String,
    pub captured_date: String,
}

/// A built profile: the JSON text plus what was derived on the way.
#[derive(Debug, Clone)]
pub struct BuiltProfile {
    pub json: String,
    pub ja3: String,
    pub ja3_string: String,
    pub ja4: String,
    pub ja4_r: String,
    pub akamai: String,
    pub permuted: bool,
    pub extensions_order: Vec<u16>,
    /// Things a human should read (dropped settings, unknown extensions).
    pub notes: Vec<String>,
}

fn q(s: &str) -> String {
    serde_json::to_string(s).expect("string serializes")
}

fn entries(out: &mut String, key: &str, items: &[(String, u16)], hex: bool, last: bool) {
    let _ = writeln!(out, "  {}: [", q(key));
    for (i, (name, id)) in items.iter().enumerate() {
        let sep = if i + 1 == items.len() { "" } else { "," };
        if hex {
            let _ = writeln!(
                out,
                "    {{ \"name\": {}, \"id\": {id}, \"hex\": \"0x{id:04X}\" }}{sep}",
                q(name)
            );
        } else {
            let _ = writeln!(out, "    {{ \"name\": {}, \"id\": {id} }}{sep}", q(name));
        }
    }
    let _ = writeln!(out, "  ]{}", if last { "" } else { "," });
}

fn str_list(v: &[String]) -> String {
    format!(
        "[{}]",
        v.iter().map(|s| q(s)).collect::<Vec<_>>().join(", ")
    )
}

fn h2_fields(h: &H2Capture, notes: &mut Vec<String>) -> String {
    let mut out = String::new();
    let known: Vec<(&str, u32)> = h
        .settings
        .iter()
        .filter_map(|&(id, v)| match setting_name(id) {
            Some(n) => Some((n, v)),
            None => {
                notes.push(format!("dropped unknown/GREASE H2 setting 0x{id:04x}={v}"));
                None
            }
        })
        .collect();
    out.push_str("  \"http2_settings\": {\n    \"ordered_params\": [\n");
    for (i, (n, v)) in known.iter().enumerate() {
        let sep = if i + 1 == known.len() { "" } else { "," };
        let _ = writeln!(out, "      {{ \"id\": {}, \"value\": {v} }}{sep}", q(n));
    }
    out.push_str("    ]");
    for (n, v) in &known {
        let _ = write!(out, ",\n    {}: {v}", q(n));
    }
    out.push_str("\n  },\n");
    let _ = writeln!(
        out,
        "  \"http2_window_update\": {},",
        h.window_update.unwrap_or(0)
    );
    let _ = writeln!(
        out,
        "  \"http2_pseudo_header_order\": {},",
        str_list(&h.pseudo_header_order)
    );
    let note = match (&h.headers_priority, h.priority_frames.is_empty()) {
        (Some(p), true) => format!(
            "No PRIORITY frames. The first HEADERS frame carries priority: exclusive={}, depends_on={}, weight={}.",
            u8::from(p.exclusive),
            p.depends_on,
            p.weight as u16 + 1
        ),
        (None, true) => "No PRIORITY frames and no HEADERS priority.".into(),
        (_, false) => "PRIORITY frames sent before the first request; see frames.".into(),
    };
    let _ = write!(out, "  \"http2_priority\": {{\n    \"note\": {}", q(&note));
    if let Some(p) = &h.headers_priority {
        let _ = write!(
            out,
            ",\n    \"headers\": {{ \"exclusive\": {}, \"depends_on\": {}, \"weight\": {} }}",
            p.exclusive,
            p.depends_on,
            p.weight as u16 + 1
        );
    }
    if !h.priority_frames.is_empty() {
        out.push_str(",\n    \"frames\": [\n");
        for (i, p) in h.priority_frames.iter().enumerate() {
            let sep = if i + 1 == h.priority_frames.len() {
                ""
            } else {
                ","
            };
            let _ = writeln!(
                out,
                "      {{ \"stream_id\": {}, \"depends_on\": {}, \"weight\": {}, \"exclusive\": {} }}{sep}",
                p.stream, p.depends_on, p.weight, p.exclusive
            );
        }
        out.push_str("    ]");
    }
    out.push_str("\n  }\n");
    out
}

/// Builds the profile from one or more captures of the same browser.
///
/// Resumed hellos are skipped. The first full hello provides the TLS values
/// and the first capture with an H2 request the H2 values. With two or more
/// hellos, the
/// extension orders are compared: if they differ but the sets match, the
/// browser permutes, `permute_extensions` is set and Chrome's canonical order
/// is written; otherwise the observed order is written.
pub fn build_profile(caps: &[Capture], meta: &ProfileMeta) -> Result<BuiltProfile, String> {
    let hellos: Vec<&ClientHello> = caps
        .iter()
        .filter_map(|c| c.client_hello.as_ref())
        .filter(|c| !is_resumption(c))
        .collect();
    let ch = *hellos
        .first()
        .ok_or("no full (non-resumed) ClientHello captured")?;
    let h2 = caps
        .iter()
        .find_map(|c| c.h2.as_ref().filter(|h| !h.pseudo_header_order.is_empty()))
        .ok_or("no capture with an HTTP/2 request (ALPN was not h2?)")?;
    if h2.pseudo_header_order.len() != 4 {
        return Err(format!(
            "expected 4 pseudo-headers, got {:?}",
            h2.pseudo_header_order
        ));
    }
    let mut notes = Vec::new();

    let check = compare_orders(&hellos);
    if !check.same_set {
        notes.push("extension SETS differ between captures; using the first capture's set".into());
    }
    let observed = extension_order(ch);
    let permuted = check.permuted && check.same_set;
    let order = if permuted {
        canonical_chrome_order(&observed, &mut notes)
    } else {
        if hellos.len() < 2 {
            notes.push("only one capture: permutation not checked, observed order written".into());
        }
        observed
    };

    // JA3 over the canonical order (a permuting browser has a new JA3 per
    // connection). JA4 sorts, so it is stable.
    let mut canon = ch.clone();
    canon.extensions = order
        .iter()
        .map(|id| {
            ch.extensions
                .iter()
                .find(|e| e.id == *id)
                .cloned()
                .unwrap_or(Extension {
                    id: *id,
                    data: vec![],
                })
        })
        .collect();
    let ja3_s = ja3_string(&canon);
    let ja3 = ja3_hash(&ja3_s);
    let j4 = Ja4::from_client_hello(ch, 't');
    let akamai = h2.akamai();

    let nog = |v: &&u16| !is_grease(**v);
    let ciphers: Vec<(String, u16)> = ch
        .cipher_suites
        .iter()
        .filter(nog)
        .map(|&c| (cipher_name(c), c))
        .collect();
    let exts: Vec<(String, u16)> = order.iter().map(|&e| (extension_name(e), e)).collect();
    let groups: Vec<(String, u16)> = ch
        .supported_groups
        .iter()
        .filter(nog)
        .map(|&g| (group_name(g), g))
        .collect();
    let points: Vec<(String, u16)> = ch
        .ec_point_formats
        .iter()
        .map(|&p| (point_format_name(p).to_owned(), p as u16))
        .collect();
    let sigalgs: Vec<(String, u16)> = ch
        .signature_algorithms
        .iter()
        .filter(nog)
        .map(|&s| (sigalg_name(s), s))
        .collect();
    let cc: Vec<String> = ch
        .compress_certificate
        .iter()
        .map(|&c| cert_compression_name(c))
        .collect();

    let mut o = String::from("{\n");
    let _ = writeln!(o, "  \"profile_id\": {},", q(&meta.profile_id));
    let _ = writeln!(o, "  \"description\": {},", q(&meta.description));
    let _ = writeln!(o, "  \"captured_date\": {},", q(&meta.captured_date));
    let _ = writeln!(o, "  \"ja3\": {},", q(&ja3));
    let _ = writeln!(o, "  \"ja3_string\": {},", q(&ja3_s));
    let _ = writeln!(o, "  \"ja4\": {},", q(&j4.hashed()));
    let _ = writeln!(o, "  \"ja4_r\": {},", q(&j4.raw()));
    let _ = writeln!(
        o,
        "  \"tls_version\": {},",
        q(&tls_version_name(ch.max_version()))
    );
    let _ = writeln!(
        o,
        "  \"tls_min_version\": {},",
        q(&tls_version_name(ch.min_version()))
    );
    let _ = writeln!(
        o,
        "  \"tls_max_version\": {},",
        q(&tls_version_name(ch.max_version()))
    );
    entries(&mut o, "cipher_suites", &ciphers, true, false);
    entries(&mut o, "extensions_order", &exts, true, false);
    entries(&mut o, "supported_groups", &groups, true, false);
    entries(&mut o, "ec_point_formats", &points, false, false);
    entries(&mut o, "signature_algorithms", &sigalgs, true, false);
    let _ = writeln!(o, "  \"alpn_protocols\": {},", str_list(&ch.alpn));
    let _ = writeln!(
        o,
        "  \"compress_certificate_algorithms\": {},",
        str_list(&cc)
    );
    let _ = writeln!(
        o,
        "  \"session_ticket\": {},",
        ch.has_extension(ext::SESSION_TICKET)
    );
    let _ = writeln!(o, "  \"grease\": {},", ch.uses_grease());
    let _ = writeln!(o, "  \"permute_extensions\": {permuted},");
    let _ = writeln!(o, "  \"alps_protocols\": {},", str_list(&ch.alps_protocols));
    let _ = writeln!(o, "  \"ech_grease\": {},", ch.has_extension(ext::ECH));
    // server_padding (4832): a u16 size. trust_anchors (0xCA34): a u16 length
    // and the ID list, which lb-net wants without that length.
    let ext_data = |id: u16| ch.extensions.iter().find(|e| e.id == id).map(|e| &e.data);
    if let Some(d) = ext_data(ext::SERVER_PADDING)
        && d.len() == 2
    {
        let _ = writeln!(
            o,
            "  \"server_padding_request\": {},",
            u16::from_be_bytes([d[0], d[1]])
        );
    }
    if let Some(d) = ext_data(ext::TRUST_ANCHORS)
        && d.len() >= 2
    {
        let _ = writeln!(o, "  \"trust_anchor_ids\": {},", q(&hex::encode(&d[2..])));
    }
    let _ = writeln!(o, "  \"http2_akamai_fingerprint\": {},", q(&akamai));
    o.push_str(&h2_fields(h2, &mut notes));
    o.push_str("}\n");

    if let Some(cp) = ch.alps_codepoint {
        notes.push(format!(
            "ALPS sent on codepoint {cp} ({})",
            extension_name(cp)
        ));
    }
    Ok(BuiltProfile {
        json: o,
        ja3,
        ja3_string: ja3_s,
        ja4: j4.hashed(),
        ja4_r: j4.raw(),
        akamai,
        permuted,
        extensions_order: order,
        notes,
    })
}

/// Today's UTC date as `YYYY-MM-DD`, from the system clock.
pub fn utc_date_today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    civil_date((secs / 86_400) as i64)
}

/// Days since 1970-01-01 to `YYYY-MM-DD` (Howard Hinnant's algorithm).
pub fn civil_date(days: i64) -> String {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clienthello::build;
    use crate::h2::Priority;
    use serde_json::Value;

    const FIREFOX: &str = include_str!("../../../profiles/tls/firefox_128_win10.json");

    fn chrome_like(order: &[u16]) -> ClientHello {
        let mut ids = vec![0x0a0a];
        ids.extend_from_slice(order);
        ids.push(0x1a1a);
        let ch = ClientHello {
            version: 0x0303,
            cipher_suites: vec![0x2a2a, 0x1301, 0x1302, 0x1303, 0xc02b],
            compression_methods: vec![0],
            extensions: ids
                .into_iter()
                .map(|id| {
                    let data = match id {
                        ext::SERVER_NAME => build::sni("fptest.localhost"),
                        ext::SUPPORTED_GROUPS => build::u16_vec16(&[0x3a3a, 4588, 29, 23, 24]),
                        ext::EC_POINT_FORMATS => vec![1, 0],
                        ext::SIGNATURE_ALGORITHMS => build::u16_vec16(&[0x0403, 0x0804, 0x0401]),
                        ext::ALPN => build::protocols(&["h2", "http/1.1"]),
                        ext::SUPPORTED_VERSIONS => build::u16_vec8(&[0x4a4a, 0x0304, 0x0303]),
                        ext::KEY_SHARE => build::key_share(&[0x3a3a, 29]),
                        ext::COMPRESS_CERTIFICATE => vec![2, 0, 2],
                        ext::ALPS_NEW => build::protocols(&["h2"]),
                        ext::PSK_KEY_EXCHANGE_MODES => vec![1, 1],
                        ext::RENEGOTIATION_INFO => vec![0],
                        ext::SERVER_PADDING => vec![0, 0],
                        // u16 length, then two IDs: [0x01, 0xaa] and [0x02, 0xbb, 0xcc].
                        ext::TRUST_ANCHORS => vec![0, 5, 1, 0xaa, 2, 0xbb, 0xcc],
                        _ => vec![],
                    };
                    Extension { id, data }
                })
                .collect(),
            ..Default::default()
        };
        ClientHello::parse_records(&ch.to_record()).unwrap()
    }

    fn capture(ch: ClientHello) -> Capture {
        Capture {
            peer: "127.0.0.1:1".parse().unwrap(),
            raw_client_hello: ch.to_record(),
            client_hello: Some(ch),
            negotiated_alpn: Some("h2".into()),
            h2: Some(H2Capture {
                settings: vec![(1, 65536), (2, 0), (4, 6291456), (6, 262144), (0x0a0a, 7)],
                window_update: Some(15663105),
                priority_frames: vec![],
                headers_priority: Some(Priority {
                    stream: 1,
                    exclusive: true,
                    depends_on: 0,
                    weight: 255,
                }),
                pseudo_header_order: vec![
                    ":method".into(),
                    ":authority".into(),
                    ":scheme".into(),
                    ":path".into(),
                ],
                header_names: vec![],
                frame_order: vec![],
            }),
            h1: None,
            error: None,
        }
    }

    fn meta() -> ProfileMeta {
        ProfileMeta {
            profile_id: "chrome_test_linux".into(),
            description: "test".into(),
            captured_date: "2026-11-02".into(),
        }
    }

    const CANON: [u16; 16] = [
        0, 23, 65281, 10, 11, 35, 16, 5, 13, 18, 51, 45, 43, 27, 17613, 65037,
    ];

    #[test]
    fn permuted_captures_write_canonical_order() {
        let mut shuffled = CANON;
        shuffled.reverse();
        let caps = [
            capture(chrome_like(&shuffled)),
            capture(chrome_like(&CANON[..])),
        ];
        let b = build_profile(&caps, &meta()).unwrap();
        assert!(b.permuted);
        assert_eq!(b.extensions_order, CANON);

        let v: Value = serde_json::from_str(&b.json).unwrap();
        assert_eq!(v["permute_extensions"], true);
        assert_eq!(v["grease"], true);
        assert_eq!(v["ech_grease"], true);
        assert_eq!(v["alps_protocols"][0], "h2");
        assert_eq!(v["compress_certificate_algorithms"][0], "brotli");
        assert_eq!(v["supported_groups"][0]["name"], "X25519MLKEM768");
        assert_eq!(v["cipher_suites"].as_array().unwrap().len(), 4);
        assert_eq!(v["http2_window_update"], 15663105);
        assert_eq!(
            v["http2_settings"]["ordered_params"]
                .as_array()
                .unwrap()
                .len(),
            4
        );
        assert_eq!(v["http2_settings"]["INITIAL_WINDOW_SIZE"], 6291456);
        assert!(b.notes.iter().any(|n| n.contains("0x0a0a")));
        assert_eq!(v["ja3"], ja3_hash(v["ja3_string"].as_str().unwrap()));
        assert!(
            v["ja3_string"]
                .as_str()
                .unwrap()
                .contains(",0-23-65281-10-")
        );
        assert!(v["ja4"].as_str().unwrap().starts_with("t13d0416h2_"));
        assert_eq!(
            v["http2_akamai_fingerprint"],
            "1:65536;2:0;4:6291456;6:262144;2570:7|15663105|0|m,a,s,p"
        );
    }

    #[test]
    fn records_new_chrome_extension_payloads_and_headers_priority() {
        let mut order = CANON.to_vec();
        order.insert(15, ext::TRUST_ANCHORS);
        order.insert(16, ext::SERVER_PADDING);
        let caps = [capture(chrome_like(&order))];
        let b = build_profile(&caps, &meta()).unwrap();
        let v: Value = serde_json::from_str(&b.json).unwrap();
        assert_eq!(v["server_padding_request"], 0);
        assert_eq!(v["trust_anchor_ids"], "01aa02bbcc");
        assert_eq!(v["http2_priority"]["headers"]["exclusive"], true);
        assert_eq!(v["http2_priority"]["headers"]["depends_on"], 0);
        assert_eq!(v["http2_priority"]["headers"]["weight"], 256);
        assert_eq!(v["extensions_order"][16]["name"], "server_padding");
    }

    #[test]
    fn same_keys_as_firefox_profile() {
        let caps = [capture(chrome_like(&CANON))];
        let b = build_profile(&caps, &meta()).unwrap();
        assert!(!b.permuted);
        let ours: Value = serde_json::from_str(&b.json).unwrap();
        let ff: Value = serde_json::from_str(FIREFOX).unwrap();
        for k in ff.as_object().unwrap().keys() {
            assert!(ours.get(k).is_some(), "missing key {k}");
        }
        for k in [
            "grease",
            "permute_extensions",
            "alps_protocols",
            "ech_grease",
        ] {
            assert!(ours.get(k).is_some(), "missing key {k}");
        }
    }

    #[test]
    fn committed_chrome_profile_is_consistent() {
        const CHROME: &str = include_str!("../../../profiles/tls/chrome_153_linux.json");
        let v: Value = serde_json::from_str(CHROME).unwrap();
        let ja3s = v["ja3_string"].as_str().unwrap();
        assert_eq!(v["ja3"], ja3_hash(ja3s));
        // ja4 is the hash of ja4_r.
        let raw = v["ja4_r"].as_str().unwrap();
        let (a, rest) = raw.split_once('_').unwrap();
        let (c, e) = rest.split_once('_').unwrap();
        let hashed = format!(
            "{a}_{}_{}",
            crate::fingerprint::sha256_12(c),
            crate::fingerprint::sha256_12(e)
        );
        assert_eq!(v["ja4"], hashed.as_str());
        // extensions_order, ja3 extension field and ja4_r agree.
        let order: Vec<u16> = v["extensions_order"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["id"].as_u64().unwrap() as u16)
            .collect();
        let ja3_exts: Vec<u16> = ja3s
            .split(',')
            .nth(2)
            .unwrap()
            .split('-')
            .map(|x| x.parse().unwrap())
            .collect();
        assert_eq!(order, ja3_exts);
        let mut no_sni_alpn: Vec<String> = order
            .iter()
            .filter(|&&x| x != 0 && x != 16)
            .map(|x| format!("{x:04x}"))
            .collect();
        no_sni_alpn.sort();
        assert_eq!(e.split('_').next().unwrap(), no_sni_alpn.join(","));
        assert_eq!(
            a,
            format!(
                "t13d{:02}{:02}h2",
                v["cipher_suites"].as_array().unwrap().len(),
                order.len()
            )
        );
        assert_eq!(v["permute_extensions"], true);
        assert_eq!(v["grease"], true);
        assert_eq!(v["http2_pseudo_header_order"][1], ":authority");
        // Every H2 setting has a name lb-net knows.
        for p in v["http2_settings"]["ordered_params"].as_array().unwrap() {
            let id = p["id"].as_str().unwrap();
            assert!((1..=9).any(|c| setting_name(c) == Some(id)), "{id}");
        }
    }

    #[test]
    fn civil_dates() {
        assert_eq!(civil_date(0), "1970-01-01");
        assert_eq!(civil_date(20_394), "2025-11-02");
        assert_eq!(civil_date(11_016), "2000-02-29");
    }
}

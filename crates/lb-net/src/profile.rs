//! TLS + HTTP/2 fingerprint profile, loaded from `profiles/tls/*.json`.
//!
//! The format is the one stealth-browser captured with tshark
//! (`firefox_128_*.json`). Chrome profiles use the same shape. Everything that
//! shapes the ClientHello or the H2 connection preface lives here, so a new
//! browser version is a new JSON file, not new code.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::NetError;

/// A cipher suite, TLS extension, named group or signature algorithm as the
/// capture lists it: human name plus IANA id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IanaEntry {
    pub name: String,
    pub id: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hex: Option<String>,
}

/// One HTTP/2 SETTINGS parameter, in the order the browser sends it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct H2Setting {
    /// Name as in RFC 9113, e.g. `HEADER_TABLE_SIZE`, `INITIAL_WINDOW_SIZE`.
    pub id: String,
    pub value: u32,
}

impl H2Setting {
    /// RFC 9113 identifier for the setting, if it is a known one.
    pub fn code(&self) -> Option<u16> {
        Some(match self.id.as_str() {
            "HEADER_TABLE_SIZE" => 0x1,
            "ENABLE_PUSH" => 0x2,
            "MAX_CONCURRENT_STREAMS" => 0x3,
            "INITIAL_WINDOW_SIZE" => 0x4,
            "MAX_FRAME_SIZE" => 0x5,
            "MAX_HEADER_LIST_SIZE" => 0x6,
            "ENABLE_CONNECT_PROTOCOL" => 0x8,
            "NO_RFC7540_PRIORITIES" => 0x9,
            _ => return None,
        })
    }
}

/// `http2_settings` in the JSON: an ordered list plus flat duplicates that the
/// capture tool wrote for convenience. Only `ordered_params` is used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct H2Settings {
    pub ordered_params: Vec<H2Setting>,
    #[serde(flatten)]
    pub flat: serde_json::Map<String, serde_json::Value>,
}

/// Optional PRIORITY frame description. Firefox 128 and Chrome don't send
/// RFC 7540 PRIORITY frames; older profiles may.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct H2Priority {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frames: Vec<H2PriorityFrame>,
    /// Priority block on request HEADERS frames (PRIORITY flag set). Chrome
    /// sends `exclusive: true, depends_on: 0, weight: 256`; Firefox 128 none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<H2HeadersPriority>,
}

/// RFC 7540 priority block carried on a HEADERS frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct H2HeadersPriority {
    #[serde(default)]
    pub exclusive: bool,
    #[serde(default)]
    pub depends_on: u32,
    /// RFC weight, 1 to 256 (the wire byte plus one).
    pub weight: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct H2PriorityFrame {
    pub stream_id: u32,
    pub depends_on: u32,
    pub weight: u8,
    #[serde(default)]
    pub exclusive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientHelloProfile {
    pub profile_id: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub captured_date: String,

    /// Expected fingerprints, used by the tests, never to shape the hello.
    pub ja3: String,
    pub ja3_string: String,
    #[serde(default)]
    pub ja4: String,

    pub tls_version: String,
    pub tls_min_version: String,
    pub tls_max_version: String,

    /// In wire order.
    pub cipher_suites: Vec<IanaEntry>,
    /// In wire order. Chrome permutes this per connection; see
    /// [`ClientHelloProfile::permutes_extensions`].
    pub extensions_order: Vec<IanaEntry>,
    pub supported_groups: Vec<IanaEntry>,
    #[serde(default)]
    pub ec_point_formats: Vec<IanaEntry>,
    pub signature_algorithms: Vec<IanaEntry>,
    pub alpn_protocols: Vec<String>,
    /// `"brotli"`, `"zlib"`, `"zstd"`.
    #[serde(default)]
    pub compress_certificate_algorithms: Vec<String>,
    #[serde(default)]
    pub session_ticket: bool,

    /// Chrome-only knobs. Absent in Firefox captures.
    #[serde(default)]
    pub grease: bool,
    #[serde(default)]
    pub permute_extensions: bool,
    /// ALPS (`application_settings`) protocols, Chrome sends `["h2"]`.
    #[serde(default)]
    pub alps_protocols: Vec<String>,
    #[serde(default)]
    pub ech_grease: bool,
    /// `server_padding` (4832) request size in bytes. Sent only when
    /// `extensions_order` lists 4832; absent means 0 (what Chrome 153 sends).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_padding_request: Option<u16>,
    /// `trust_anchors` (0xCA34): the trust anchor ID list as hex, in wire form
    /// (8-bit length-prefixed IDs, without the outer 16-bit length). Sent only
    /// when `extensions_order` lists 51764; absent means an empty list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust_anchor_ids: Option<String>,

    pub http2_settings: H2Settings,
    pub http2_window_update: u32,
    /// e.g. `[":method", ":authority", ":scheme", ":path"]` for Chrome.
    pub http2_pseudo_header_order: Vec<String>,
    #[serde(default)]
    pub http2_priority: H2Priority,
}

impl ClientHelloProfile {
    pub fn from_json(json: &str) -> Result<Self, NetError> {
        let p: Self = serde_json::from_str(json)?;
        p.validate()?;
        Ok(p)
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self, NetError> {
        let json = std::fs::read_to_string(path).map_err(|e| NetError::Profile(e.to_string()))?;
        Self::from_json(&json)
    }

    /// Chrome shuffles extension order per connection. Firefox keeps it fixed.
    pub fn permutes_extensions(&self) -> bool {
        self.permute_extensions
    }

    /// Family this profile belongs to, inferred from the id prefix.
    pub fn family(&self) -> Option<lb_persona::BrowserFamily> {
        self.profile_id.split('_').next()?.parse().ok()
    }

    /// Minimal sanity checks so a bad profile fails at load, not mid-handshake.
    fn validate(&self) -> Result<(), NetError> {
        if self.cipher_suites.is_empty() {
            return Err(NetError::Profile("no cipher_suites".into()));
        }
        if self.alpn_protocols.is_empty() {
            return Err(NetError::Profile("no alpn_protocols".into()));
        }
        if self.http2_pseudo_header_order.len() != 4 {
            return Err(NetError::Profile(
                "http2_pseudo_header_order must list :method :authority :scheme :path".into(),
            ));
        }
        self.trust_anchor_ids_bytes()?;
        if let Some(h) = &self.http2_priority.headers
            && (!(1..=256).contains(&h.weight) || h.depends_on >= 1 << 31)
        {
            return Err(NetError::Profile(format!(
                "http2_priority.headers out of range: {h:?}"
            )));
        }
        for s in &self.http2_settings.ordered_params {
            if s.code().is_none() {
                return Err(NetError::Profile(format!("unknown H2 setting {}", s.id)));
            }
        }
        Ok(())
    }

    /// [`Self::trust_anchor_ids`] decoded and checked: each ID is a non-empty,
    /// 8-bit length-prefixed string. Absent gives an empty list.
    pub fn trust_anchor_ids_bytes(&self) -> Result<Vec<u8>, NetError> {
        let Some(hex) = &self.trust_anchor_ids else {
            return Ok(Vec::new());
        };
        let bad = || NetError::Profile(format!("trust_anchor_ids is not valid hex: {hex:?}"));
        if hex.len() % 2 != 0 {
            return Err(bad());
        }
        let bytes = (0..hex.len())
            .step_by(2)
            .map(|i| {
                hex.get(i..i + 2)
                    .and_then(|b| u8::from_str_radix(b, 16).ok())
                    .ok_or_else(bad)
            })
            .collect::<Result<Vec<u8>, _>>()?;
        let mut i = 0;
        while i < bytes.len() {
            let len = usize::from(bytes[i]);
            if len == 0 || i + 1 + len > bytes.len() {
                return Err(NetError::Profile(
                    "trust_anchor_ids: IDs must be non-empty and length-prefixed".into(),
                ));
            }
            i += 1 + len;
        }
        Ok(bytes)
    }

    /// Hex ids of the cipher suites in wire order, e.g. `["1301", "1302", ...]`.
    pub fn cipher_hex(&self) -> impl Iterator<Item = String> + '_ {
        self.cipher_suites.iter().map(|c| format!("{:04x}", c.id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIREFOX_WIN10: &str = include_str!("../../../profiles/tls/firefox_128_win10.json");

    #[test]
    fn loads_firefox_capture() {
        let p = ClientHelloProfile::from_json(FIREFOX_WIN10).unwrap();
        assert_eq!(p.profile_id, "firefox_128_win10");
        assert_eq!(p.family(), Some(lb_persona::BrowserFamily::Firefox));
        assert_eq!(p.cipher_suites.len(), 17);
        assert_eq!(p.cipher_suites[0].id, 0x1301);
        assert_eq!(p.http2_pseudo_header_order[1], ":path");
        assert_eq!(p.http2_settings.ordered_params[0].id, "HEADER_TABLE_SIZE");
        assert_eq!(p.http2_window_update, 12_517_377);
        assert!(!p.permutes_extensions());
    }

    #[test]
    fn trust_anchor_ids_are_checked() {
        let mut p = ClientHelloProfile::from_json(FIREFOX_WIN10).unwrap();
        assert_eq!(p.trust_anchor_ids_bytes().unwrap(), Vec::<u8>::new());
        p.trust_anchor_ids = Some("0201ff01AA".into());
        assert_eq!(
            p.trust_anchor_ids_bytes().unwrap(),
            vec![2, 1, 0xff, 1, 0xaa]
        );
        for bad in ["0", "zz", "00", "0301ff"] {
            p.trust_anchor_ids = Some(bad.into());
            assert!(p.trust_anchor_ids_bytes().is_err(), "{bad}");
        }
    }

    #[test]
    fn round_trips() {
        let p = ClientHelloProfile::from_json(FIREFOX_WIN10).unwrap();
        let again: ClientHelloProfile =
            serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(p, again);
    }

    #[test]
    fn ja3_string_matches_cipher_list() {
        let p = ClientHelloProfile::from_json(FIREFOX_WIN10).unwrap();
        let ciphers: Vec<String> = p.cipher_suites.iter().map(|c| c.id.to_string()).collect();
        let field = p.ja3_string.split(',').nth(1).unwrap();
        assert_eq!(field, ciphers.join("-"));
    }
}

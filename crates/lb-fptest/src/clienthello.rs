//! TLS ClientHello parser: raw record bytes in, structured hello out.
//!
//! Handles a ClientHello split over several TLS records. Keeps every
//! extension (id and raw body) in wire order, and decodes the ones that
//! fingerprints use.

use std::fmt;

/// Extension ids this module decodes.
pub mod ext {
    pub const SERVER_NAME: u16 = 0;
    pub const STATUS_REQUEST: u16 = 5;
    pub const SUPPORTED_GROUPS: u16 = 10;
    pub const EC_POINT_FORMATS: u16 = 11;
    pub const SIGNATURE_ALGORITHMS: u16 = 13;
    pub const ALPN: u16 = 16;
    pub const SCT: u16 = 18;
    pub const PADDING: u16 = 21;
    pub const EXTENDED_MASTER_SECRET: u16 = 23;
    pub const COMPRESS_CERTIFICATE: u16 = 27;
    pub const RECORD_SIZE_LIMIT: u16 = 28;
    pub const DELEGATED_CREDENTIALS: u16 = 34;
    pub const SESSION_TICKET: u16 = 35;
    pub const PRE_SHARED_KEY: u16 = 41;
    pub const SUPPORTED_VERSIONS: u16 = 43;
    pub const PSK_KEY_EXCHANGE_MODES: u16 = 45;
    pub const KEY_SHARE: u16 = 51;
    /// ALPS, old codepoint (Chrome up to ~130).
    pub const ALPS_OLD: u16 = 17513;
    /// ALPS, new codepoint (Chrome 131+).
    pub const ALPS_NEW: u16 = 17613;
    pub const ECH: u16 = 65037;
    pub const RENEGOTIATION_INFO: u16 = 65281;
    /// Server padding request (Chrome 153).
    pub const SERVER_PADDING: u16 = 4832;
    /// draft-ietf-tls-trust-anchor-ids (Chrome 153).
    pub const TRUST_ANCHORS: u16 = 51764;
}

/// RFC 8701 GREASE value: `0x?A?A` with both bytes equal.
pub fn is_grease(v: u16) -> bool {
    (v & 0x0f0f) == 0x0a0a && (v >> 8) == (v & 0xff)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extension {
    pub id: u16,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClientHello {
    /// Version in the first record header (usually 0x0301).
    pub record_version: u16,
    /// `legacy_version` in the hello body (0x0303 for TLS 1.2 and 1.3).
    pub version: u16,
    pub random: [u8; 32],
    pub session_id: Vec<u8>,
    /// Wire order, GREASE included.
    pub cipher_suites: Vec<u16>,
    pub compression_methods: Vec<u8>,
    /// Wire order, GREASE included.
    pub extensions: Vec<Extension>,

    // Decoded extensions. Lists keep wire order and GREASE values.
    pub server_name: Option<String>,
    pub supported_groups: Vec<u16>,
    pub ec_point_formats: Vec<u8>,
    pub signature_algorithms: Vec<u16>,
    pub alpn: Vec<String>,
    pub supported_versions: Vec<u16>,
    pub key_share_groups: Vec<u16>,
    pub psk_key_exchange_modes: Vec<u8>,
    /// Certificate compression algorithm ids (1 zlib, 2 brotli, 3 zstd).
    pub compress_certificate: Vec<u16>,
    /// ALPS protocols and which codepoint carried them.
    pub alps_protocols: Vec<String>,
    pub alps_codepoint: Option<u16>,
    pub record_size_limit: Option<u16>,
    pub delegated_credentials: Vec<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// More bytes are needed to finish the hello.
    Incomplete,
    Invalid(&'static str),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Incomplete => f.write_str("incomplete ClientHello"),
            Self::Invalid(m) => write!(f, "invalid ClientHello: {m}"),
        }
    }
}

impl std::error::Error for ParseError {}

/// Big-endian cursor over a byte slice.
struct Reader<'a> {
    b: &'a [u8],
}

impl<'a> Reader<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self { b }
    }
    fn take(&mut self, n: usize, what: &'static str) -> Result<&'a [u8], ParseError> {
        if self.b.len() < n {
            return Err(ParseError::Invalid(what));
        }
        let (h, t) = self.b.split_at(n);
        self.b = t;
        Ok(h)
    }
    fn u8(&mut self, what: &'static str) -> Result<u8, ParseError> {
        Ok(self.take(1, what)?[0])
    }
    fn u16(&mut self, what: &'static str) -> Result<u16, ParseError> {
        let s = self.take(2, what)?;
        Ok(u16::from_be_bytes([s[0], s[1]]))
    }
    fn vec8(&mut self, what: &'static str) -> Result<&'a [u8], ParseError> {
        let n = self.u8(what)? as usize;
        self.take(n, what)
    }
    fn vec16(&mut self, what: &'static str) -> Result<&'a [u8], ParseError> {
        let n = self.u16(what)? as usize;
        self.take(n, what)
    }
    fn is_empty(&self) -> bool {
        self.b.is_empty()
    }
}

fn u16_list(b: &[u8], what: &'static str) -> Result<Vec<u16>, ParseError> {
    if !b.len().is_multiple_of(2) {
        return Err(ParseError::Invalid(what));
    }
    Ok(b.chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect())
}

fn protocol_list(b: &[u8], what: &'static str) -> Result<Vec<String>, ParseError> {
    let mut r = Reader::new(b);
    let mut list = Reader::new(r.vec16(what)?);
    let mut out = Vec::new();
    while !list.is_empty() {
        out.push(String::from_utf8_lossy(list.vec8(what)?).into_owned());
    }
    Ok(out)
}

/// Joins the handshake fragments of consecutive handshake records.
///
/// Returns the handshake message bytes (type + length + body) once a whole
/// ClientHello is present, and the number of input bytes it took.
pub fn reassemble(records: &[u8]) -> Result<(Vec<u8>, usize, u16), ParseError> {
    let mut hs = Vec::new();
    let mut pos = 0;
    let mut record_version = 0;
    loop {
        if records.len() < pos + 5 {
            return Err(ParseError::Incomplete);
        }
        let h = &records[pos..pos + 5];
        if h[0] != 0x16 {
            return Err(ParseError::Invalid("not a handshake record"));
        }
        if pos == 0 {
            record_version = u16::from_be_bytes([h[1], h[2]]);
        }
        let len = u16::from_be_bytes([h[3], h[4]]) as usize;
        if records.len() < pos + 5 + len {
            return Err(ParseError::Incomplete);
        }
        hs.extend_from_slice(&records[pos + 5..pos + 5 + len]);
        pos += 5 + len;
        if hs.len() >= 4 {
            if hs[0] != 0x01 {
                return Err(ParseError::Invalid(
                    "first handshake message is not ClientHello",
                ));
            }
            let body = u32::from_be_bytes([0, hs[1], hs[2], hs[3]]) as usize;
            if hs.len() >= 4 + body {
                hs.truncate(4 + body);
                return Ok((hs, pos, record_version));
            }
        }
    }
}

impl ClientHello {
    /// Parses one or more TLS records holding a ClientHello.
    pub fn parse_records(records: &[u8]) -> Result<Self, ParseError> {
        let (hs, _, record_version) = reassemble(records)?;
        let mut ch = Self::parse_handshake(&hs)?;
        ch.record_version = record_version;
        Ok(ch)
    }

    /// Parses a handshake message (`01 LL LL LL` + body).
    pub fn parse_handshake(hs: &[u8]) -> Result<Self, ParseError> {
        let mut r = Reader::new(hs);
        if r.u8("handshake type")? != 0x01 {
            return Err(ParseError::Invalid("not a ClientHello"));
        }
        let len = r.take(3, "handshake length")?;
        let len = u32::from_be_bytes([0, len[0], len[1], len[2]]) as usize;
        let body = r.take(len, "handshake body")?;
        Self::parse_body(body)
    }

    fn parse_body(body: &[u8]) -> Result<Self, ParseError> {
        let mut r = Reader::new(body);
        let mut ch = ClientHello {
            version: r.u16("legacy_version")?,
            ..Default::default()
        };
        ch.random.copy_from_slice(r.take(32, "random")?);
        ch.session_id = r.vec8("session_id")?.to_vec();
        ch.cipher_suites = u16_list(r.vec16("cipher_suites")?, "cipher_suites")?;
        ch.compression_methods = r.vec8("compression_methods")?.to_vec();
        if r.is_empty() {
            return Ok(ch);
        }
        let mut exts = Reader::new(r.vec16("extensions")?);
        while !exts.is_empty() {
            let id = exts.u16("extension id")?;
            let data = exts.vec16("extension body")?.to_vec();
            ch.decode_extension(id, &data)?;
            ch.extensions.push(Extension { id, data });
        }
        Ok(ch)
    }

    fn decode_extension(&mut self, id: u16, d: &[u8]) -> Result<(), ParseError> {
        let mut r = Reader::new(d);
        match id {
            ext::SERVER_NAME if !d.is_empty() => {
                let mut list = Reader::new(r.vec16("sni list")?);
                while !list.is_empty() {
                    let kind = list.u8("sni type")?;
                    let name = list.vec16("sni name")?;
                    if kind == 0 && self.server_name.is_none() {
                        self.server_name = Some(String::from_utf8_lossy(name).into_owned());
                    }
                }
            }
            ext::SUPPORTED_GROUPS => {
                self.supported_groups = u16_list(r.vec16("groups")?, "groups")?;
            }
            ext::EC_POINT_FORMATS => self.ec_point_formats = r.vec8("point formats")?.to_vec(),
            ext::SIGNATURE_ALGORITHMS => {
                self.signature_algorithms = u16_list(r.vec16("sigalgs")?, "sigalgs")?;
            }
            ext::ALPN => self.alpn = protocol_list(d, "alpn")?,
            ext::SUPPORTED_VERSIONS => {
                self.supported_versions = u16_list(r.vec8("versions")?, "versions")?;
            }
            ext::KEY_SHARE => {
                let mut list = Reader::new(r.vec16("key_share")?);
                while !list.is_empty() {
                    self.key_share_groups.push(list.u16("key_share group")?);
                    list.vec16("key_share key")?;
                }
            }
            ext::PSK_KEY_EXCHANGE_MODES => {
                self.psk_key_exchange_modes = r.vec8("psk modes")?.to_vec();
            }
            ext::COMPRESS_CERTIFICATE => {
                self.compress_certificate = u16_list(r.vec8("cert compression")?, "cc")?;
            }
            ext::ALPS_OLD | ext::ALPS_NEW => {
                self.alps_protocols = protocol_list(d, "alps")?;
                self.alps_codepoint = Some(id);
            }
            ext::RECORD_SIZE_LIMIT => self.record_size_limit = Some(r.u16("record_size_limit")?),
            ext::DELEGATED_CREDENTIALS => {
                self.delegated_credentials = u16_list(r.vec16("dc")?, "dc")?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Extension ids in wire order.
    pub fn extension_ids(&self) -> Vec<u16> {
        self.extensions.iter().map(|e| e.id).collect()
    }

    pub fn has_extension(&self, id: u16) -> bool {
        self.extensions.iter().any(|e| e.id == id)
    }

    /// True when any cipher, extension, group, version or key share is GREASE.
    pub fn uses_grease(&self) -> bool {
        self.cipher_suites.iter().any(|&v| is_grease(v))
            || self.extensions.iter().any(|e| is_grease(e.id))
            || self.supported_groups.iter().any(|&v| is_grease(v))
            || self.supported_versions.iter().any(|&v| is_grease(v))
            || self.key_share_groups.iter().any(|&v| is_grease(v))
    }

    /// Highest non-GREASE version in `supported_versions`, else `version`.
    pub fn max_version(&self) -> u16 {
        self.supported_versions
            .iter()
            .copied()
            .filter(|&v| !is_grease(v))
            .max()
            .unwrap_or(self.version)
    }

    /// Lowest non-GREASE version in `supported_versions`, else `version`.
    pub fn min_version(&self) -> u16 {
        self.supported_versions
            .iter()
            .copied()
            .filter(|&v| !is_grease(v))
            .min()
            .unwrap_or(self.version)
    }

    /// Serializes back to one handshake record. Used to build test vectors.
    pub fn to_record(&self) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&self.version.to_be_bytes());
        body.extend_from_slice(&self.random);
        body.push(self.session_id.len() as u8);
        body.extend_from_slice(&self.session_id);
        body.extend_from_slice(&((self.cipher_suites.len() * 2) as u16).to_be_bytes());
        for c in &self.cipher_suites {
            body.extend_from_slice(&c.to_be_bytes());
        }
        body.push(self.compression_methods.len() as u8);
        body.extend_from_slice(&self.compression_methods);
        let mut exts = Vec::new();
        for e in &self.extensions {
            exts.extend_from_slice(&e.id.to_be_bytes());
            exts.extend_from_slice(&(e.data.len() as u16).to_be_bytes());
            exts.extend_from_slice(&e.data);
        }
        body.extend_from_slice(&(exts.len() as u16).to_be_bytes());
        body.extend_from_slice(&exts);

        let mut hs = vec![0x01];
        hs.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
        hs.extend_from_slice(&body);
        let mut rec = vec![0x16];
        let rv = if self.record_version == 0 {
            0x0301
        } else {
            self.record_version
        };
        rec.extend_from_slice(&rv.to_be_bytes());
        rec.extend_from_slice(&(hs.len() as u16).to_be_bytes());
        rec.extend_from_slice(&hs);
        rec
    }
}

/// Builders for extension bodies, used by tests and test vectors.
pub mod build {
    pub fn u16_vec16(v: &[u16]) -> Vec<u8> {
        let mut o = ((v.len() * 2) as u16).to_be_bytes().to_vec();
        v.iter().for_each(|x| o.extend_from_slice(&x.to_be_bytes()));
        o
    }
    pub fn u16_vec8(v: &[u16]) -> Vec<u8> {
        let mut o = vec![(v.len() * 2) as u8];
        v.iter().for_each(|x| o.extend_from_slice(&x.to_be_bytes()));
        o
    }
    pub fn protocols(p: &[&str]) -> Vec<u8> {
        let mut l = Vec::new();
        for s in p {
            l.push(s.len() as u8);
            l.extend_from_slice(s.as_bytes());
        }
        let mut o = (l.len() as u16).to_be_bytes().to_vec();
        o.extend(l);
        o
    }
    pub fn sni(host: &str) -> Vec<u8> {
        let mut entry = vec![0u8];
        entry.extend_from_slice(&(host.len() as u16).to_be_bytes());
        entry.extend_from_slice(host.as_bytes());
        let mut o = (entry.len() as u16).to_be_bytes().to_vec();
        o.extend(entry);
        o
    }
    /// key_share with a dummy 32-byte key per group.
    pub fn key_share(groups: &[u16]) -> Vec<u8> {
        let mut l = Vec::new();
        for g in groups {
            l.extend_from_slice(&g.to_be_bytes());
            l.extend_from_slice(&32u16.to_be_bytes());
            l.extend_from_slice(&[0x42; 32]);
        }
        let mut o = (l.len() as u16).to_be_bytes().to_vec();
        o.extend(l);
        o
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ClientHello {
        let mut ch = ClientHello {
            record_version: 0x0301,
            version: 0x0303,
            random: [7; 32],
            session_id: vec![1; 32],
            cipher_suites: vec![0x1a1a, 0x1301, 0x1302, 0xc02b],
            compression_methods: vec![0],
            ..Default::default()
        };
        let exts: Vec<(u16, Vec<u8>)> = vec![
            (0x2a2a, vec![]),
            (ext::SERVER_NAME, build::sni("localhost")),
            (ext::SUPPORTED_GROUPS, build::u16_vec16(&[0x3a3a, 29, 23])),
            (ext::EC_POINT_FORMATS, vec![1, 0]),
            (
                ext::SIGNATURE_ALGORITHMS,
                build::u16_vec16(&[0x0403, 0x0804]),
            ),
            (ext::ALPN, build::protocols(&["h2", "http/1.1"])),
            (
                ext::SUPPORTED_VERSIONS,
                build::u16_vec8(&[0x4a4a, 0x0304, 0x0303]),
            ),
            (ext::KEY_SHARE, build::key_share(&[0x3a3a, 29])),
            (ext::COMPRESS_CERTIFICATE, vec![2, 0, 2]),
            (ext::ALPS_NEW, build::protocols(&["h2"])),
            (0x5a5a, vec![0]),
        ];
        ch.extensions = exts
            .into_iter()
            .map(|(id, data)| Extension { id, data })
            .collect();
        ch
    }

    #[test]
    fn grease_detection() {
        for v in [0x0a0a, 0x1a1a, 0xfafa] {
            assert!(is_grease(v));
        }
        for v in [0x0a1a, 0x1301, 0x0000, 0xff01] {
            assert!(!is_grease(v));
        }
    }

    #[test]
    fn round_trip_and_decode() {
        let rec = sample().to_record();
        let ch = ClientHello::parse_records(&rec).unwrap();
        assert_eq!(ch.server_name.as_deref(), Some("localhost"));
        assert_eq!(ch.supported_groups, vec![0x3a3a, 29, 23]);
        assert_eq!(ch.ec_point_formats, vec![0]);
        assert_eq!(ch.signature_algorithms, vec![0x0403, 0x0804]);
        assert_eq!(ch.alpn, vec!["h2", "http/1.1"]);
        assert_eq!(ch.supported_versions, vec![0x4a4a, 0x0304, 0x0303]);
        assert_eq!(ch.key_share_groups, vec![0x3a3a, 29]);
        assert_eq!(ch.compress_certificate, vec![2]);
        assert_eq!(ch.alps_protocols, vec!["h2"]);
        assert_eq!(ch.alps_codepoint, Some(ext::ALPS_NEW));
        assert_eq!(ch.extension_ids()[0], 0x2a2a);
        assert!(ch.uses_grease());
        assert_eq!(ch.max_version(), 0x0304);
        assert_eq!(ch.min_version(), 0x0303);
        assert_eq!(ch.to_record(), rec);
    }

    #[test]
    fn split_over_records_and_incomplete() {
        let rec = sample().to_record();
        let hs = &rec[5..];
        let (a, b) = hs.split_at(40);
        let mut two = Vec::new();
        for frag in [a, b] {
            two.extend_from_slice(&[0x16, 0x03, 0x01]);
            two.extend_from_slice(&(frag.len() as u16).to_be_bytes());
            two.extend_from_slice(frag);
        }
        assert_eq!(
            ClientHello::parse_records(&two).unwrap(),
            ClientHello::parse_records(&rec).unwrap()
        );
        assert_eq!(
            ClientHello::parse_records(&two[..60]),
            Err(ParseError::Incomplete)
        );
        assert!(matches!(
            ClientHello::parse_records(b"GET / HTTP/1.1\r\n"),
            Err(ParseError::Invalid(_))
        ));
    }
}

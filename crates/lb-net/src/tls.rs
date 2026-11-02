//! BoringSSL connector shaped by a [`ClientHelloProfile`].
//!
//! [`TlsConnector::new`] builds one `SslConnector` per profile. Every
//! ClientHello field comes from the profile, never from globals:
//!
//! | ClientHello field | Profile field | BoringSSL knob |
//! |---|---|---|
//! | TLS 1.3 cipher order | `cipher_suites` (0x13xx) | `SSL_CTX_set1_tls13_ciphers` (FFI) |
//! | TLS 1.2 cipher order | `cipher_suites` (rest) | `set_strict_cipher_list` |
//! | supported_groups | `supported_groups` | `set_curves_list` |
//! | signature_algorithms | `signature_algorithms` | `set_sigalgs_list` |
//! | ALPN | `alpn_protocols` | `set_alpn_protos` |
//! | supported_versions | `tls_min_version`, `tls_max_version` | `set_min/max_proto_version` |
//! | GREASE | `grease` | `set_grease_enabled` |
//! | extension shuffle | `permute_extensions` | `set_permute_extensions` |
//! | compress_certificate (27) | `compress_certificate_algorithms` | `add_certificate_compression_algorithm` |
//! | status_request (5) | `extensions_order` has 5 | `enable_ocsp_stapling` |
//! | signed_certificate_timestamp (18) | `extensions_order` has 18 | `enable_signed_cert_timestamps` |
//! | session_ticket (35) | `session_ticket` | `SslOptions::NO_TICKET` when false |
//! | ALPS (17513 / 17613) | `alps_protocols`, codepoint from `extensions_order` | `SSL_add_application_settings` (FFI, per connection) |
//! | ECH GREASE (65037) | `ech_grease` | `set_enable_ech_grease` (per connection) |
//! | server_name | the `host` passed to [`TlsConnector::connect`] | SNI, skipped for IP literals |
//!
//! Cipher suites: BoringSSL always writes the TLS 1.3 suites before the
//! TLS 1.2 ones. Both browsers do the same, so this costs nothing. The TLS 1.3
//! order is set exactly through FFI (Firefox uses 1301, 1303, 1302). Suites
//! BoringSSL does not implement (DHE, CBC-SHA384, ...) are skipped and listed
//! in [`TlsConnector::gaps`]; BoringSSL itself would silently ignore unknown
//! names in a non-strict list, so we filter first and then use the strict
//! setter, which fails loudly if our table is wrong.
//!
//! Known gaps, accepted for M1 (closed in M4b with small BoringSSL patches):
//!
//! - **record_size_limit (28)** and **delegated_credentials (34)**: Firefox
//!   sends both. Stock BoringSSL cannot emit either on the client side.
//! - **Extension order**: BoringSSL writes extensions in its own fixed order
//!   (or a random one with `permute_extensions`). Firefox's fixed order cannot
//!   be reproduced, so Firefox JA3 differs; JA4 sorts extensions and is only
//!   affected by the two missing extensions above.
//! - **ffdhe2048 / ffdhe3072 (256, 257)**: BoringSSL has no finite-field
//!   groups. Skipped, listed in [`TlsConnector::gaps`].
//! - **X25519Kyber768Draft00 (0x6399)**: removed from this BoringSSL. Profiles
//!   should use X25519MLKEM768 (0x11ec), which is supported.
//! - **zstd certificate compression**: `boring` has no algorithm id for it.
//! - **key_share groups**: BoringSSL picks them (first group, plus the second
//!   one when the first is post-quantum). Firefox sends x25519 + P-256; the
//!   profile has no field for this yet.
//! - **pre_shared_key (41)**: no client session cache yet, so no resumption.
//!
//! Certificate verification is on by default against the system roots
//! BoringSSL finds through its default verify paths. Set
//! [`TlsConnector::danger_accept_invalid_certs`] only for our local test
//! servers with self-signed certificates.

use std::io::{self, Read, Write};
use std::sync::Arc;

use boring::ssl::{
    CertificateCompressionAlgorithm, CertificateCompressor, ConnectConfiguration, SslConnector,
    SslMethod, SslOptions, SslVerifyMode, SslVersion,
};
use foreign_types::ForeignTypeRef;
use tokio::net::TcpStream;

use crate::HttpVersion;
use crate::error::NetError;
use crate::profile::ClientHelloProfile;

/// IANA extension id of ALPS with the old (Chrome < 131) codepoint.
const EXT_ALPS_OLD: u16 = 17513;
/// IANA extension id of ALPS with the new codepoint.
const EXT_ALPS_NEW: u16 = 17613;
const EXT_STATUS_REQUEST: u16 = 5;
const EXT_SCT: u16 = 18;

/// A reusable TLS configuration derived from one profile.
pub struct TlsConnector {
    profile: Arc<ClientHelloProfile>,
    connector: SslConnector,
    /// ALPS protocols, validated to be in the ALPN list.
    alps: Vec<Vec<u8>>,
    alps_new_codepoint: bool,
    gaps: Vec<String>,
    /// Skip certificate chain and hostname checks. Only for our local test
    /// servers with self-signed certificates. Default `false`.
    pub danger_accept_invalid_certs: bool,
}

impl TlsConnector {
    pub fn new(profile: Arc<ClientHelloProfile>) -> Result<Self, NetError> {
        let mut gaps = Vec::new();
        let connector = build_connector(&profile, &mut gaps)?;

        let mut alps = Vec::new();
        for proto in &profile.alps_protocols {
            if !profile.alpn_protocols.contains(proto) {
                return Err(NetError::Profile(format!(
                    "ALPS protocol {proto:?} is not in alpn_protocols"
                )));
            }
            alps.push(proto.as_bytes().to_vec());
        }
        let has_ext = |id: u16| profile.extensions_order.iter().any(|e| e.id == id);
        let alps_new_codepoint = !(has_ext(EXT_ALPS_OLD) && !has_ext(EXT_ALPS_NEW));

        for g in &gaps {
            tracing::debug!(profile = %profile.profile_id, "tls gap: {g}");
        }

        Ok(Self {
            profile,
            connector,
            alps,
            alps_new_codepoint,
            gaps,
            danger_accept_invalid_certs: false,
        })
    }

    /// Builder-style setter for [`Self::danger_accept_invalid_certs`].
    pub fn with_danger_accept_invalid_certs(mut self, accept: bool) -> Self {
        self.danger_accept_invalid_certs = accept;
        self
    }

    pub fn profile(&self) -> &ClientHelloProfile {
        &self.profile
    }

    /// Profile entries BoringSSL cannot emit and that were skipped, e.g.
    /// `"supported_groups: ffdhe2048 (256)"`. Empty means everything the
    /// profile lists is configured (the fixed gaps in the module docs still
    /// apply).
    pub fn gaps(&self) -> &[String] {
        &self.gaps
    }

    /// Handshake over `tcp` for `host`. Returns the stream and the HTTP
    /// version chosen through ALPN (`h2` gives [`HttpVersion::Http2`], anything
    /// else or no ALPN gives [`HttpVersion::Http1`]).
    ///
    /// `host` is used for SNI and hostname verification. IP literals (IPv6
    /// with or without brackets) get no SNI, as in browsers.
    pub async fn connect(
        &self,
        host: &str,
        tcp: TcpStream,
    ) -> Result<(tokio_boring::SslStream<TcpStream>, HttpVersion), NetError> {
        let config = self.configure()?;
        let host = host.trim_start_matches('[').trim_end_matches(']');
        let stream = tokio_boring::connect(config, host, tcp)
            .await
            .map_err(|e| NetError::Tls(format!("handshake with {host}: {e}")))?;
        let version = match stream.ssl().selected_alpn_protocol() {
            Some(b"h2") => HttpVersion::Http2,
            _ => HttpVersion::Http1,
        };
        Ok((stream, version))
    }

    /// Per-connection settings that BoringSSL only exposes on `SSL`.
    fn configure(&self) -> Result<ConnectConfiguration, NetError> {
        let mut config = self.connector.configure()?;
        if self.danger_accept_invalid_certs {
            config.set_verify(SslVerifyMode::NONE);
            config.set_verify_hostname(false);
        }
        config.set_enable_ech_grease(self.profile.ech_grease);
        if !self.alps.is_empty() {
            let ssl = ForeignTypeRef::as_ptr(&*config);
            // SAFETY: `ssl` is a valid, live SSL owned by `config` for the
            // whole block. BoringSSL copies `proto` and `settings` before
            // returning, so the borrowed slices only need to outlive the call.
            // `boring` 5.x does not wrap ALPS, hence the raw FFI.
            unsafe {
                boring_sys::SSL_set_alps_use_new_codepoint(ssl, i32::from(self.alps_new_codepoint));
                for proto in &self.alps {
                    let settings: &[u8] = &[];
                    let ok = boring_sys::SSL_add_application_settings(
                        ssl,
                        proto.as_ptr(),
                        proto.len(),
                        settings.as_ptr(),
                        settings.len(),
                    );
                    if ok != 1 {
                        return Err(boring::error::ErrorStack::get().into());
                    }
                }
            }
        }
        Ok(config)
    }
}

/// Builds the shared `SSL_CTX`. Anything skipped is pushed to `gaps`.
fn build_connector(
    p: &ClientHelloProfile,
    gaps: &mut Vec<String>,
) -> Result<SslConnector, NetError> {
    // Verifies peers against the default verify paths (system roots).
    let mut b = SslConnector::builder(SslMethod::tls())?;

    let min = parse_version(&p.tls_min_version)?;
    let max = parse_version(&p.tls_max_version)?;
    b.set_min_proto_version(Some(min))?;
    b.set_max_proto_version(Some(max))?;

    // Cipher suites.
    let mut tls13: Vec<u16> = Vec::new();
    let mut tls12: Vec<&'static str> = Vec::new();
    for c in &p.cipher_suites {
        if is_grease(c.id) {
            continue; // GREASE comes from the `grease` flag.
        }
        if is_tls13_cipher(c.id) {
            tls13.push(c.id);
        } else if let Some(name) = cipher_name(c.id) {
            tls12.push(name);
        } else {
            gaps.push(format!("cipher_suites: {} ({:#06x})", c.name, c.id));
        }
    }
    if !tls13.is_empty() {
        let ctx = b.as_ptr();
        // SAFETY: `ctx` is the live SSL_CTX owned by `b`. BoringSSL copies the
        // id array; a null `flags` pointer is allowed. `boring` 5.x does not
        // wrap this setter, and without it BoringSSL uses its own TLS 1.3
        // order, which differs from Firefox's.
        let ok = unsafe {
            boring_sys::SSL_CTX_set1_tls13_ciphers(
                ctx,
                tls13.as_ptr(),
                std::ptr::null(),
                tls13.len(),
            )
        };
        if ok != 1 {
            return Err(boring::error::ErrorStack::get().into());
        }
    }
    if tls12.is_empty() {
        if min != SslVersion::TLS1_3 {
            return Err(NetError::Profile(
                "no usable TLS 1.2 cipher suites but tls_min_version is below TLS 1.3".into(),
            ));
        }
    } else {
        b.set_strict_cipher_list(&tls12.join(":"))?;
    }

    // Groups.
    let mut curves: Vec<&'static str> = Vec::new();
    for g in &p.supported_groups {
        if is_grease(g.id) {
            continue;
        }
        match group_name(g.id) {
            Some(n) => curves.push(n),
            None => gaps.push(format!("supported_groups: {} ({})", g.name, g.id)),
        }
    }
    if !curves.is_empty() {
        b.set_curves_list(&curves.join(":"))?;
    }

    // Signature algorithms. For a client without a certificate only the
    // verify list matters; the signing list set alongside it is unused.
    let mut sigalgs: Vec<&'static str> = Vec::new();
    for s in &p.signature_algorithms {
        if is_grease(s.id) {
            continue;
        }
        match sigalg_name(s.id) {
            Some(n) => sigalgs.push(n),
            None => gaps.push(format!("signature_algorithms: {} ({:#06x})", s.name, s.id)),
        }
    }
    if !sigalgs.is_empty() {
        b.set_sigalgs_list(&sigalgs.join(":"))?;
    }

    b.set_alpn_protos(&alpn_wire(&p.alpn_protocols)?)?;

    b.set_grease_enabled(p.grease);
    b.set_permute_extensions(p.permute_extensions);

    let has_ext = |id: u16| p.extensions_order.iter().any(|e| e.id == id);
    if has_ext(EXT_STATUS_REQUEST) {
        b.enable_ocsp_stapling();
    }
    if has_ext(EXT_SCT) {
        b.enable_signed_cert_timestamps();
    }
    if !p.session_ticket {
        b.set_options(SslOptions::NO_TICKET);
    }

    // Wire order of compress_certificate follows registration order.
    for alg in &p.compress_certificate_algorithms {
        match alg.as_str() {
            "brotli" => b.add_certificate_compression_algorithm(BrotliDecompressor)?,
            "zlib" => b.add_certificate_compression_algorithm(ZlibDecompressor)?,
            other => gaps.push(format!("compress_certificate_algorithms: {other}")),
        }
    }

    Ok(b.build())
}

/// Accepts `"TLS 1.2"`, `"TLSv1.2"`, `"1.2"` and the like.
fn parse_version(s: &str) -> Result<SslVersion, NetError> {
    let digits: String = s
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    Ok(match digits.trim_matches('.') {
        "1" | "1.0" => SslVersion::TLS1,
        "1.1" => SslVersion::TLS1_1,
        "1.2" => SslVersion::TLS1_2,
        "1.3" => SslVersion::TLS1_3,
        _ => return Err(NetError::Profile(format!("unknown TLS version {s:?}"))),
    })
}

/// ALPN wire format: each protocol prefixed by its length byte.
fn alpn_wire(protos: &[String]) -> Result<Vec<u8>, NetError> {
    let mut out = Vec::new();
    for proto in protos {
        let len = u8::try_from(proto.len())
            .ok()
            .filter(|l| *l > 0)
            .ok_or_else(|| NetError::Profile(format!("bad ALPN protocol {proto:?}")))?;
        out.push(len);
        out.extend_from_slice(proto.as_bytes());
    }
    Ok(out)
}

/// RFC 8701 GREASE values: 0x0a0a, 0x1a1a, ..., 0xfafa.
fn is_grease(id: u16) -> bool {
    id & 0x0f0f == 0x0a0a && (id >> 8) == (id & 0xff)
}

fn is_tls13_cipher(id: u16) -> bool {
    matches!(id, 0x1301..=0x1303)
}

/// IANA id to BoringSSL's OpenSSL-style name for TLS 1.0–1.2 suites.
/// Covers Firefox 128 and Chrome. Suites BoringSSL lacks return `None`.
fn cipher_name(id: u16) -> Option<&'static str> {
    Some(match id {
        0xc02b => "ECDHE-ECDSA-AES128-GCM-SHA256",
        0xc02f => "ECDHE-RSA-AES128-GCM-SHA256",
        0xc02c => "ECDHE-ECDSA-AES256-GCM-SHA384",
        0xc030 => "ECDHE-RSA-AES256-GCM-SHA384",
        0xcca9 => "ECDHE-ECDSA-CHACHA20-POLY1305",
        0xcca8 => "ECDHE-RSA-CHACHA20-POLY1305",
        0xc009 => "ECDHE-ECDSA-AES128-SHA",
        0xc00a => "ECDHE-ECDSA-AES256-SHA",
        0xc013 => "ECDHE-RSA-AES128-SHA",
        0xc014 => "ECDHE-RSA-AES256-SHA",
        0xc023 => "ECDHE-ECDSA-AES128-SHA256",
        0xc027 => "ECDHE-RSA-AES128-SHA256",
        0x009c => "AES128-GCM-SHA256",
        0x009d => "AES256-GCM-SHA384",
        0x002f => "AES128-SHA",
        0x0035 => "AES256-SHA",
        0x000a => "DES-CBC3-SHA",
        _ => return None,
    })
}

/// IANA named group id to BoringSSL curve name.
fn group_name(id: u16) -> Option<&'static str> {
    Some(match id {
        29 => "X25519",
        23 => "P-256",
        24 => "P-384",
        25 => "P-521",
        0x11ec => "X25519MLKEM768",
        0x0202 => "MLKEM1024",
        // 0x6399 X25519Kyber768Draft00 and ffdhe* are not in this BoringSSL.
        _ => return None,
    })
}

/// IANA SignatureScheme id to BoringSSL sigalg name.
fn sigalg_name(id: u16) -> Option<&'static str> {
    Some(match id {
        0x0201 => "rsa_pkcs1_sha1",
        0x0401 => "rsa_pkcs1_sha256",
        0x0501 => "rsa_pkcs1_sha384",
        0x0601 => "rsa_pkcs1_sha512",
        0x0203 => "ecdsa_sha1",
        0x0403 => "ecdsa_secp256r1_sha256",
        0x0503 => "ecdsa_secp384r1_sha384",
        0x0603 => "ecdsa_secp521r1_sha512",
        0x0804 => "rsa_pss_rsae_sha256",
        0x0805 => "rsa_pss_rsae_sha384",
        0x0806 => "rsa_pss_rsae_sha512",
        0x0807 => "ed25519",
        _ => return None,
    })
}

/// Decompress-only brotli for `compress_certificate`. The client never
/// compresses its own certificate.
struct BrotliDecompressor;

impl CertificateCompressor for BrotliDecompressor {
    const ALGORITHM: CertificateCompressionAlgorithm = CertificateCompressionAlgorithm::BROTLI;
    const CAN_COMPRESS: bool = false;
    const CAN_DECOMPRESS: bool = true;

    fn decompress<W: Write>(&self, input: &[u8], output: &mut W) -> io::Result<()> {
        copy_all(brotli::Decompressor::new(input, 4096), output)
    }
}

/// Decompress-only zlib for `compress_certificate`.
struct ZlibDecompressor;

impl CertificateCompressor for ZlibDecompressor {
    const ALGORITHM: CertificateCompressionAlgorithm = CertificateCompressionAlgorithm::ZLIB;
    const CAN_COMPRESS: bool = false;
    const CAN_DECOMPRESS: bool = true;

    fn decompress<W: Write>(&self, input: &[u8], output: &mut W) -> io::Result<()> {
        copy_all(flate2::read::ZlibDecoder::new(input), output)
    }
}

fn copy_all<R: Read, W: Write>(mut r: R, w: &mut W) -> io::Result<()> {
    io::copy(&mut r, w).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::IanaEntry;
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;

    const FIREFOX_WIN10: &str = include_str!("../../../profiles/tls/firefox_128_win10.json");

    fn firefox() -> ClientHelloProfile {
        ClientHelloProfile::from_json(FIREFOX_WIN10).unwrap()
    }

    fn entries(ids: &[u16]) -> Vec<IanaEntry> {
        ids.iter()
            .map(|&id| IanaEntry {
                name: format!("{id:#06x}"),
                id,
                hex: None,
            })
            .collect()
    }

    /// Chrome-like profile: GREASE, permutation, ALPS, ECH GREASE, PQ group.
    fn chrome_like() -> ClientHelloProfile {
        let mut p = firefox();
        p.profile_id = "chrome_131_synthetic".into();
        p.cipher_suites = entries(&[
            0x1301, 0x1302, 0x1303, 0xc02b, 0xc02f, 0xc02c, 0xc030, 0xcca9, 0xcca8, 0xc013, 0xc014,
            0x009c, 0x009d, 0x002f, 0x0035,
        ]);
        p.extensions_order = entries(&[
            0, 23, 65281, 10, 11, 35, 16, 5, 13, 18, 51, 45, 43, 27, 17613, 65037,
        ]);
        p.supported_groups = entries(&[0x11ec, 29, 23, 24]);
        p.signature_algorithms = entries(&[
            0x0403, 0x0804, 0x0401, 0x0503, 0x0805, 0x0501, 0x0806, 0x0601,
        ]);
        p.tls_min_version = "TLS 1.2".into();
        p.grease = true;
        p.permute_extensions = true;
        p.alps_protocols = vec!["h2".into()];
        p.ech_grease = true;
        p.compress_certificate_algorithms = vec!["brotli".into()];
        p
    }

    /// What we read back from a captured ClientHello.
    #[derive(Debug, Default)]
    struct Hello {
        ciphers: Vec<u16>,
        extensions: Vec<u16>,
        groups: Vec<u16>,
        sigalgs: Vec<u16>,
        alpn: Vec<String>,
        cert_compression: Vec<u16>,
        versions: Vec<u16>,
        sni: Option<String>,
    }

    fn u16_at(b: &[u8], i: usize) -> u16 {
        u16::from_be_bytes([b[i], b[i + 1]])
    }

    fn u16_list(b: &[u8]) -> Vec<u16> {
        b.chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect()
    }

    /// Minimal ClientHello parser for tests. `body` starts at the handshake
    /// header.
    fn parse_hello(body: &[u8]) -> Hello {
        assert_eq!(body[0], 1, "not a ClientHello");
        let mut i = 4 + 2 + 32; // handshake header, legacy_version, random
        i += 1 + body[i] as usize; // session id
        let n = u16_at(body, i) as usize;
        let ciphers = u16_list(&body[i + 2..i + 2 + n]);
        i += 2 + n;
        i += 1 + body[i] as usize; // compression methods
        let ext_end = i + 2 + u16_at(body, i) as usize;
        i += 2;
        let mut h = Hello {
            ciphers,
            ..Default::default()
        };
        while i < ext_end {
            let ty = u16_at(body, i);
            let len = u16_at(body, i + 2) as usize;
            let data = &body[i + 4..i + 4 + len];
            h.extensions.push(ty);
            match ty {
                0 => h.sni = Some(String::from_utf8(data[5..].to_vec()).unwrap()),
                10 => h.groups = u16_list(&data[2..]),
                13 => h.sigalgs = u16_list(&data[2..]),
                16 => {
                    let mut j = 2;
                    while j < data.len() {
                        let l = data[j] as usize;
                        h.alpn
                            .push(String::from_utf8(data[j + 1..j + 1 + l].to_vec()).unwrap());
                        j += 1 + l;
                    }
                }
                27 => h.cert_compression = u16_list(&data[1..]),
                43 => h.versions = u16_list(&data[1..]),
                _ => {}
            }
            i += 4 + len;
        }
        h
    }

    /// Runs one handshake against a local listener that records the
    /// ClientHello and hangs up. Loopback only.
    async fn capture(connector: &TlsConnector, host: &str) -> Hello {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = async {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut hdr = [0u8; 5];
            sock.read_exact(&mut hdr).await.unwrap();
            assert_eq!(hdr[0], 22, "not a handshake record");
            let len = u16::from_be_bytes([hdr[3], hdr[4]]) as usize;
            let mut body = vec![0u8; len];
            sock.read_exact(&mut body).await.unwrap();
            body
        };
        let client = async {
            let tcp = TcpStream::connect(addr).await.unwrap();
            // The server hangs up, so the handshake must fail.
            assert!(connector.connect(host, tcp).await.is_err());
        };
        let (body, ()) = tokio::join!(server, client);
        parse_hello(&body)
    }

    #[test]
    fn builds_from_firefox_profile() {
        let c = TlsConnector::new(Arc::new(firefox())).unwrap();
        assert!(!c.danger_accept_invalid_certs);
        // Only the ffdhe groups are unsupported in the Firefox capture.
        assert_eq!(c.gaps().len(), 2, "{:?}", c.gaps());
        assert!(c.gaps().iter().all(|g| g.contains("ffdhe")));
    }

    #[test]
    fn builds_from_chrome_like_profile() {
        let c = TlsConnector::new(Arc::new(chrome_like()))
            .unwrap()
            .with_danger_accept_invalid_certs(true);
        assert!(c.gaps().is_empty(), "{:?}", c.gaps());
        assert!(c.danger_accept_invalid_certs);
        assert!(c.alps_new_codepoint);
    }

    #[test]
    fn rejects_alps_outside_alpn() {
        let mut p = chrome_like();
        p.alps_protocols = vec!["h3".into()];
        assert!(TlsConnector::new(Arc::new(p)).is_err());
    }

    #[test]
    fn cipher_table_matches_boringssl() {
        for id in [
            0xc02b, 0xc02f, 0xc02c, 0xc030, 0xcca9, 0xcca8, 0xc009, 0xc00a, 0xc013, 0xc014, 0xc023,
            0xc027, 0x009c, 0x009d, 0x002f, 0x0035, 0x000a,
        ] {
            let name = cipher_name(id).unwrap();
            let c = boring::ssl::SslCipher::from_value(id)
                .unwrap_or_else(|| panic!("{id:#06x} not in BoringSSL"));
            assert_eq!(c.name(), name, "{id:#06x}");
        }
    }

    #[test]
    fn parses_versions() {
        assert_eq!(parse_version("TLS 1.0").unwrap(), SslVersion::TLS1);
        assert_eq!(parse_version("TLSv1.2").unwrap(), SslVersion::TLS1_2);
        assert_eq!(parse_version("1.3").unwrap(), SslVersion::TLS1_3);
        assert!(parse_version("SSL 3.0").is_err());
    }

    #[test]
    fn grease_detection() {
        assert!(is_grease(0x0a0a));
        assert!(is_grease(0xfafa));
        assert!(!is_grease(0x1a2a));
        assert!(!is_grease(0x1301));
    }

    #[test]
    fn decompressors_round_trip() {
        let data = b"certificate bytes ".repeat(100);

        let mut br = Vec::new();
        {
            let mut w = brotli::CompressorWriter::new(&mut br, 4096, 5, 22);
            w.write_all(&data).unwrap();
        }
        let mut out = Vec::new();
        BrotliDecompressor.decompress(&br, &mut out).unwrap();
        assert_eq!(out, data);

        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(&data).unwrap();
        let z = z.finish().unwrap();
        let mut out = Vec::new();
        ZlibDecompressor.decompress(&z, &mut out).unwrap();
        assert_eq!(out, data);
    }

    #[tokio::test]
    async fn firefox_client_hello_matches_profile() {
        let p = firefox();
        let c = TlsConnector::new(Arc::new(p.clone())).unwrap();
        let h = capture(&c, "localhost").await;

        let want: Vec<u16> = p.cipher_suites.iter().map(|c| c.id).collect();
        assert_eq!(h.ciphers, want, "cipher order");
        assert_eq!(h.groups, vec![29, 23, 24, 25], "groups minus ffdhe");
        let want: Vec<u16> = p.signature_algorithms.iter().map(|s| s.id).collect();
        assert_eq!(h.sigalgs, want);
        assert_eq!(h.alpn, p.alpn_protocols);
        assert_eq!(h.cert_compression, vec![2]);
        assert_eq!(h.sni.as_deref(), Some("localhost"));
        assert_eq!(h.versions.first(), Some(&0x0304));
        assert!(!h.ciphers.iter().any(|&c| is_grease(c)));

        // Every emitted extension is in the profile (padding aside), and every
        // profile extension is emitted except the documented gaps.
        let profile_ext: Vec<u16> = p.extensions_order.iter().map(|e| e.id).collect();
        for e in &h.extensions {
            assert!(*e == 21 || profile_ext.contains(e), "unexpected ext {e}");
        }
        for e in &profile_ext {
            let gap = matches!(e, 21 | 28 | 34 | 41);
            assert!(gap || h.extensions.contains(e), "missing ext {e}");
        }
    }

    #[tokio::test]
    async fn chrome_like_client_hello_has_grease_alps_ech() {
        let p = chrome_like();
        let c = TlsConnector::new(Arc::new(p.clone())).unwrap();
        let h = capture(&c, "localhost").await;

        assert!(is_grease(h.ciphers[0]), "GREASE cipher first");
        let real: Vec<u16> = h.ciphers[1..].to_vec();
        let want: Vec<u16> = p.cipher_suites.iter().map(|c| c.id).collect();
        assert_eq!(real, want);
        assert!(is_grease(h.groups[0]));
        assert_eq!(&h.groups[1..], &[0x11ec, 29, 23, 24]);
        assert!(h.extensions.contains(&EXT_ALPS_NEW), "ALPS");
        assert!(h.extensions.contains(&65037), "ECH GREASE");
        assert!(h.extensions.contains(&EXT_STATUS_REQUEST));
        assert!(h.extensions.contains(&EXT_SCT));
        assert_eq!(h.extensions.iter().filter(|&&e| is_grease(e)).count(), 2);

        // Permutation: extension order changes between connections.
        let mut orders = vec![h.extensions];
        for _ in 0..4 {
            orders.push(capture(&c, "localhost").await.extensions);
        }
        let strip = |v: &Vec<u16>| -> Vec<u16> {
            v.iter()
                .copied()
                .filter(|e| !is_grease(*e) && *e != 21)
                .collect()
        };
        let first = strip(&orders[0]);
        assert!(orders.iter().skip(1).any(|o| strip(o) != first));
    }

    #[tokio::test]
    async fn old_alps_codepoint_from_extensions_order() {
        let mut p = chrome_like();
        for e in &mut p.extensions_order {
            if e.id == EXT_ALPS_NEW {
                e.id = EXT_ALPS_OLD;
            }
        }
        let c = TlsConnector::new(Arc::new(p)).unwrap();
        let h = capture(&c, "localhost").await;
        assert!(h.extensions.contains(&EXT_ALPS_OLD));
        assert!(!h.extensions.contains(&EXT_ALPS_NEW));
    }

    #[tokio::test]
    async fn no_sni_for_ip_literal() {
        let c = TlsConnector::new(Arc::new(firefox())).unwrap();
        let h = capture(&c, "127.0.0.1").await;
        assert_eq!(h.sni, None);
        assert!(!h.extensions.contains(&0));
    }
}

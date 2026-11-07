//! Test support for LightBrowser network fingerprints. Never shipped.
//!
//! - [`clienthello`]: TLS ClientHello parser.
//! - [`fingerprint`]: JA3, JA4 and the Akamai H2 text.
//! - [`h2`]: minimal HTTP/2 frame and HPACK reader for the client preface.
//! - [`tcp`]: client-SYN capture, parser and JA4T (Linux, needs `CAP_NET_RAW`).
//! - [`server`]: local TLS capture server (rustls, self-signed, in memory).
//! - [`site`]: local page server (HTTP/1.1 keep-alive, HTTP/2) that
//!   records which requests went over which connection.
//! - [`profile`]: turn captures into a `profiles/tls/*.json` profile.
//!
//! Only for our own local test servers and detection engine.

pub mod clienthello;
pub mod fingerprint;
pub mod h2;
pub mod profile;
pub mod server;
#[cfg(target_os = "linux")]
pub mod tcp;
pub mod site;

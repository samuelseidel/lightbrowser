//! BoringSSL connector shaped by a [`ClientHelloProfile`].
//!
//! Owner: M1 task 2. Builds an `SslConnector` whose ClientHello matches the
//! profile: cipher list, groups, signature algorithms, ALPN, min/max version,
//! GREASE, extension permutation, certificate compression, SCT and OCSP.
//! Returns a connected `tokio_boring::SslStream` and the negotiated ALPN.

use std::sync::Arc;

use tokio::net::TcpStream;

use crate::HttpVersion;
use crate::error::NetError;
use crate::profile::ClientHelloProfile;

/// A reusable TLS configuration derived from one profile.
pub struct TlsConnector {
    profile: Arc<ClientHelloProfile>,
}

impl TlsConnector {
    pub fn new(profile: Arc<ClientHelloProfile>) -> Result<Self, NetError> {
        Ok(Self { profile })
    }

    pub fn profile(&self) -> &ClientHelloProfile {
        &self.profile
    }

    /// Handshake over `tcp` for `host`. Returns the stream and the HTTP
    /// version chosen through ALPN.
    pub async fn connect(
        &self,
        _host: &str,
        _tcp: TcpStream,
    ) -> Result<(tokio_boring::SslStream<TcpStream>, HttpVersion), NetError> {
        Err(NetError::Tls(
            "tls::TlsConnector::connect not implemented (M1 task 2)".into(),
        ))
    }
}

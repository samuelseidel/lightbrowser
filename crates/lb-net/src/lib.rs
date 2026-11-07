//! Network stack: BoringSSL TLS with per-family ClientHello profiles,
//! HTTP/1.1 (own writer), HTTP/2 (forked h2 with profile-driven SETTINGS and
//! pseudo-header order), cookies, body decoding and proxies.
//!
//! Every fingerprint-relevant value comes from the context's
//! [`Arc<Persona>`](lb_persona::Persona) and its [`ClientHelloProfile`].
//! Nothing here reads globals or environment variables. Nothing writes to disk.
//!
//! Module map (see docs/ARCHITECTURE.md):
//! - [`profile`]: the TLS/H2 profile JSON (`profiles/tls/*.json`)
//! - [`tcpfp`]: TCP/IP SYN profiles (`profiles/tcp/*.json`), socket options and
//!   the packet shim that make the SYN look like Windows, macOS or Linux
//! - [`tls`]: builds a BoringSSL connector from a profile
//! - [`h1`], [`h2`]: HTTP transports over the TLS stream, picked by ALPN
//! - [`header_profile`]: per-request-kind header sets (`profiles/headers`)
//! - [`client_hints`]: `Accept-CH` / `Critical-CH` and the high-entropy
//!   `Sec-CH-UA-*` hints per origin
//! - [`headers`]: per-family navigation headers, cookie placement
//! - [`cookies`]: in-memory cookie jar
//! - [`decompress`]: gzip / brotli / zstd body decoding
//! - [`pool`]: connection reuse per browser context (HTTP/1.1 keep-alive,
//!   HTTP/2 sessions, TLS resumption; ADR 0018)
//! - [`proxy`]: HTTP `CONNECT`, forward and SOCKS5 proxies per browser
//!   context with Chrome's bypass rules (ADR 0027)
//! - [`fetch`]: the pipeline that ties the above together

pub mod client_hints;
pub mod cookies;
mod cxx_runtime;
pub mod decompress;
pub mod error;
pub mod fetch;
pub mod h1;
pub mod h2;
pub mod header_profile;
pub mod headers;
pub mod pool;
pub mod profile;
pub mod proxy;
pub mod tcpfp;
pub mod tls;
pub mod websocket;

pub use client_hints::{ClientHintsStore, UaHint};
pub use error::NetError;
pub use fetch::{RedirectHop, Request, Response, fetch};
pub use header_profile::{HeaderProfile, RequestKind, RequestMeta};
pub use pool::ConnectionPool;
pub use profile::ClientHelloProfile;
pub use proxy::{Credentials, Proxy, ProxyChallenge, ProxyKind};

/// Which HTTP version a connection negotiated (via ALPN).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpVersion {
    Http1,
    Http2,
}

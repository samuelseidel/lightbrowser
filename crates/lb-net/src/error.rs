//! Error type for the network stack.

#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("invalid url: {0}")]
    Url(#[from] url::ParseError),
    #[error("unsupported scheme: {0}")]
    UnsupportedScheme(String),
    #[error("dns: {0}")]
    Dns(String),
    #[error("connect: {0}")]
    Connect(#[from] std::io::Error),
    #[error("tls: {0}")]
    Tls(String),
    #[error("http: {0}")]
    Http(String),
    #[error("body too large (limit {limit} bytes)")]
    BodyTooLarge { limit: usize },
    #[error("decode ({encoding}): {source}")]
    Decode {
        encoding: String,
        #[source]
        source: std::io::Error,
    },
    #[error("profile: {0}")]
    Profile(String),
    #[error("persona family {persona} does not match TLS profile {profile}")]
    FamilyMismatch { persona: String, profile: String },
}

impl From<boring::error::ErrorStack> for NetError {
    fn from(e: boring::error::ErrorStack) -> Self {
        NetError::Tls(e.to_string())
    }
}

impl From<http::Error> for NetError {
    fn from(e: http::Error) -> Self {
        NetError::Http(e.to_string())
    }
}

impl From<hyper::Error> for NetError {
    fn from(e: hyper::Error) -> Self {
        NetError::Http(e.to_string())
    }
}

impl From<::h2::Error> for NetError {
    fn from(e: ::h2::Error) -> Self {
        NetError::Http(e.to_string())
    }
}

impl From<serde_json::Error> for NetError {
    fn from(e: serde_json::Error) -> Self {
        NetError::Profile(e.to_string())
    }
}
